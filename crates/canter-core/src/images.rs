use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{Cursor, Read},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use canter_model::{Message, Request, UserContent};
use canter_protocol::{
    ErrorCode, EventPayload, MAX_IMAGE_BYTES, ToolContentItem, UserInput, image_mime_type,
};
use canter_store::Store;
use image::{
    DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, imageops::FilterType,
};

use crate::{CoreError, session::Shared};

/// Only returns a replacement when the original can be kept smaller in its own format.
fn downscale(bytes: &[u8]) -> Option<Vec<u8>> {
    let format = image::guess_format(bytes).ok()?;
    match format {
        ImageFormat::Gif => return None,
        ImageFormat::WebP => {
            if image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))
                .ok()?
                .has_animation()
            {
                return None;
            }
        }
        ImageFormat::Png => {
            if image::codecs::png::PngDecoder::new(Cursor::new(bytes))
                .ok()?
                .is_apng()
                .ok()?
            {
                return None;
            }
        }
        ImageFormat::Jpeg => {}
        _ => return None,
    }
    let mut decoder = ImageReader::with_format(Cursor::new(bytes), format)
        .into_decoder()
        .ok()?;
    let (width, height) = decoder.dimensions();
    if width.max(height) <= 1568 {
        return None;
    }
    let orientation = decoder.orientation().ok()?;
    // Decoding can change the pixel color space (for example CMYK JPEG to RGB).
    let color_space: &[u8] = if decoder.color_type().has_color() {
        b"RGB "
    } else {
        b"GRAY"
    };
    let profile = decoder
        .icc_profile()
        .ok()
        .flatten()
        .filter(|profile| profile.get(16..20) == Some(color_space));
    let mut resized =
        DynamicImage::from_decoder(decoder)
            .ok()?
            .resize(1568, 1568, FilterType::Triangle);
    resized.apply_orientation(orientation);
    let mut output = Vec::new();
    match format {
        // Fast PNG encoding can outweigh the pixel savings on screenshots/PDF pages.
        ImageFormat::Png => encode(
            &resized,
            image::codecs::png::PngEncoder::new_with_quality(
                &mut output,
                image::codecs::png::CompressionType::Best,
                image::codecs::png::FilterType::Adaptive,
            ),
            profile,
        ),
        ImageFormat::Jpeg => encode(
            &resized,
            image::codecs::jpeg::JpegEncoder::new(&mut output),
            profile,
        ),
        ImageFormat::WebP => encode(
            &resized,
            image::codecs::webp::WebPEncoder::new_lossless(&mut output),
            profile,
        ),
        _ => None,
    }?;
    (output.len() < bytes.len()).then_some(output)
}

fn encode(
    image: &DynamicImage,
    mut encoder: impl ImageEncoder,
    profile: Option<Vec<u8>>,
) -> Option<()> {
    if let Some(profile) = profile {
        encoder.set_icc_profile(profile).ok()?;
    }
    image.write_with_encoder(encoder).ok()
}

fn read(path: &Path) -> Result<(Vec<u8>, &'static str), CoreError> {
    let invalid = |message| CoreError::new(ErrorCode::InvalidRequest, message);
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| invalid("cannot read image file"))?;
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Err(invalid("image path must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("cannot read image file"))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(invalid("image exceeds the 8 MiB limit"));
    }
    let mime =
        image_mime_type(&bytes).ok_or_else(|| invalid("image must be JPEG, PNG, GIF or WebP"))?;
    Ok((bytes, mime))
}

fn read_prepared(
    store: &Store,
    session: &str,
    name: &str,
    preparations: &mut Option<BTreeMap<String, String>>,
) -> Result<(Vec<u8>, &'static str), CoreError> {
    if name.starts_with(canter_store::PREPARED_IMAGE_PREFIX) {
        return read(&store.image_path(session, name)?);
    }
    let preparations = preparations.get_or_insert_with(|| store.prepared_images(session));
    if let Some(prepared) = preparations.get(name)
        && let Ok(image) = read(&store.image_path(session, prepared)?)
    {
        return Ok(image);
    }
    let (bytes, mime) = read(&store.image_path(session, name)?)?;
    let replacement = downscale(&bytes).and_then(|resized| {
        let path = store
            .save_prepared_image(session, &resized, mime.strip_prefix("image/").unwrap())
            .ok()?;
        Some((path.file_name()?.to_str()?.to_owned(), resized))
    });
    let (prepared, bytes) = replacement.unwrap_or_else(|| (name.to_owned(), bytes));
    preparations.insert(name.to_owned(), prepared);
    // Migration is best effort: unreadable metadata must not break a usable image/session.
    let _ = store.save_prepared_images(session, preparations);
    Ok((bytes, mime))
}

pub(crate) struct LoadedInput {
    input: Vec<UserInput>,
    images: Vec<LoadedImage>,
}

struct LoadedImage {
    index: usize,
    bytes: Vec<u8>,
    mime: &'static str,
    name: String,
}

/// Roll back newly written copies unless the input is accepted durably.
#[derive(Default)]
pub(crate) struct ImportedImages(Vec<PathBuf>);

impl ImportedImages {
    pub fn keep(mut self) {
        self.0.clear();
    }
}

impl Drop for ImportedImages {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(crate) async fn load(cwd: String, input: Vec<UserInput>) -> Result<LoadedInput, CoreError> {
    tokio::task::spawn_blocking(move || {
        // Validate the entire message before saving any attachment or event.
        let images = input
            .iter()
            .enumerate()
            .filter_map(|(index, part)| {
                let UserInput::LocalImage { path, name } = part else {
                    return None;
                };
                let path = Path::new(&cwd).join(path);
                Some(read(&path).map(|(bytes, mime)| LoadedImage {
                    index,
                    bytes,
                    mime,
                    name: name.clone().unwrap_or_else(|| {
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    }),
                }))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(LoadedInput { input, images })
    })
    .await
    .map_err(|_| CoreError::internal())?
}

pub(crate) async fn import(
    store: Arc<Store>,
    session: String,
    loaded: LoadedInput,
) -> Result<(Vec<UserInput>, ImportedImages), CoreError> {
    tokio::task::spawn_blocking(move || {
        let LoadedInput { mut input, images } = loaded;
        let mut saved = ImportedImages::default();
        for LoadedImage {
            index,
            bytes,
            mime,
            name,
        } in images
        {
            let resized = downscale(&bytes);
            let bytes = resized.as_deref().unwrap_or(&bytes);
            let path =
                store.save_prepared_image(&session, bytes, mime.strip_prefix("image/").unwrap())?;
            input[index] = UserInput::LocalImage {
                path: path
                    .file_name()
                    .expect("saved image filename")
                    .to_string_lossy()
                    .into_owned(),
                name: Some(name),
            };
            saved.0.push(path);
        }
        Ok((input, saved))
    })
    .await
    .map_err(|_| CoreError::internal())?
}

pub(crate) async fn import_tool(
    store: Arc<Store>,
    session: String,
    images: Vec<canter_tools::ToolImage>,
) -> Result<(Vec<ToolContentItem>, Vec<UserContent>, ImportedImages), CoreError> {
    tokio::task::spawn_blocking(move || {
        let mut saved = ImportedImages::default();
        let mut items = Vec::new();
        let mut content = Vec::new();
        for image in images {
            let resized = downscale(&image.bytes);
            let bytes = resized.as_deref().unwrap_or(&image.bytes);
            let path = store.save_prepared_image(
                &session,
                bytes,
                image.mime_type.strip_prefix("image/").unwrap(),
            )?;
            let filename = path
                .file_name()
                .expect("saved image filename")
                .to_string_lossy()
                .into_owned();
            saved.0.push(path);
            content.push(UserContent::Text {
                text: format!("[Tool image: {}]", image.name),
            });
            content.push(UserContent::Image {
                path: filename.clone(),
                name: image.name.clone(),
                data_url: String::new(),
            });
            items.push(ToolContentItem::LocalImage {
                path: filename,
                name: image.name,
                mime_type: image.mime_type.into(),
                size_bytes: bytes.len() as u64,
            });
        }
        Ok((items, content, saved))
    })
    .await
    .map_err(|_| CoreError::internal())?
}

pub(crate) fn message(input: &[UserInput]) -> Message {
    if !input
        .iter()
        .any(|part| matches!(part, UserInput::LocalImage { .. }))
    {
        return Message::User {
            text: input
                .iter()
                .filter_map(|part| match part {
                    UserInput::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
    }
    Message::UserParts {
        content: input
            .iter()
            .filter_map(|part| match part {
                UserInput::Text { text } => Some(UserContent::Text { text: text.clone() }),
                UserInput::LocalImage { path, name } => Some(UserContent::Image {
                    path: path.clone(),
                    name: name.clone().unwrap_or_default(),
                    data_url: String::new(),
                }),
                UserInput::Skill { .. } | UserInput::Unknown => None,
            })
            .collect(),
    }
}

pub(crate) async fn prepare(shared: &Shared, request: Request) -> Result<Request, CoreError> {
    let (request, warnings) = prepare_request(shared, request).await?;
    if !warnings.is_empty() {
        shared
            .state
            .lock()
            .await
            .journal
            .append(
                warnings
                    .into_iter()
                    .map(|message| EventPayload::Warning { message })
                    .collect(),
            )
            .await?;
    }
    Ok(request)
}

/// The same wire images for every request, without publishing session events.
pub(crate) async fn prepare_request(
    shared: &Shared,
    mut request: Request,
) -> Result<(Request, Vec<String>), CoreError> {
    if !request.messages.iter().any(|message| {
        matches!(
            message,
            Message::UserParts { .. } | Message::ToolImages { .. }
        )
    }) {
        return Ok((request, Vec::new()));
    }
    let accepts_images = shared
        .runtime()
        .options
        .image_models
        .contains(&request.model);
    let (store, session) = {
        let state = shared.state.lock().await;
        (
            state.journal.store.clone(),
            state.journal.snapshot.id.clone(),
        )
    };
    let preparation = shared.image_preparation.clone();
    let (request, warnings) = tokio::task::spawn_blocking(move || {
        // Ordinary, compact and side requests may prepare the same legacy image concurrently.
        let _guard = preparation
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut preparations = None;
        let mut encoded_bytes = 0;
        let mut over_budget = false;
        let mut warnings = Vec::new();
        const IMAGE_BUDGET: usize = 40 * 1024 * 1024;
        for message in request.messages.iter_mut().rev() {
            if let Message::UserParts { content } | Message::ToolImages { content } = message {
                for part in content.iter_mut().rev() {
                    if let UserContent::Image {
                        path,
                        name,
                        data_url,
                    } = part
                    {
                        if accepts_images && !over_budget {
                            let loaded = read_prepared(&store, &session, path, &mut preparations);
                            let (bytes, mime) = match loaded {
                                Ok(image) => image,
                                Err(_) => {
                                    let text = format!("[image unavailable: {name}]");
                                    warnings.push(format!("{text}: saved copy could not be read"));
                                    *part = UserContent::Text { text };
                                    continue;
                                }
                            };
                            encoded_bytes += bytes.len().div_ceil(3) * 4;
                            if encoded_bytes <= IMAGE_BUDGET {
                                *data_url = format!("data:{mime};base64,{}", base64(&bytes));
                                continue;
                            }
                            over_budget = true;
                        }
                        *part = UserContent::Text {
                            text: format!("[image omitted: {name}]"),
                        };
                    }
                }
            }
        }
        (request, warnings)
    })
    .await
    .map_err(|_| CoreError::internal())?;
    Ok((request, warnings))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as usize;
        let b = chunk.get(1).copied().unwrap_or(0) as usize;
        let c = chunk.get(2).copied().unwrap_or(0) as usize;
        result.push(ALPHABET[a >> 2] as char);
        result.push(ALPHABET[(a & 3) << 4 | b >> 4] as char);
        result.push(if chunk.len() > 1 {
            ALPHABET[(b & 15) << 2 | c >> 6] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            ALPHABET[c & 63] as char
        } else {
            '='
        });
    }
    result
}
