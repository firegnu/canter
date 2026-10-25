use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use canter_model::{Message, Request, UserContent};
use canter_protocol::{ErrorCode, EventPayload, MAX_IMAGE_BYTES, UserInput, image_mime_type};
use canter_store::Store;

use crate::{CoreError, session::Shared};

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
            let path = store.save_image(&session, &bytes, mime.strip_prefix("image/").unwrap())?;
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
                UserInput::Unknown => None,
            })
            .collect(),
    }
}

pub(crate) async fn prepare(shared: &Shared, mut request: Request) -> Result<Request, CoreError> {
    if !request
        .messages
        .iter()
        .any(|message| matches!(message, Message::UserParts { .. }))
    {
        return Ok(request);
    }
    let accepts_images = shared.options.image_models.contains(&request.model);
    let (store, session) = {
        let state = shared.state.lock().await;
        (
            state.journal.store.clone(),
            state.journal.snapshot.id.clone(),
        )
    };
    let (request, warnings) = tokio::task::spawn_blocking(move || {
        let mut encoded_bytes = 0;
        let mut over_budget = false;
        let mut warnings = Vec::new();
        const IMAGE_BUDGET: usize = 40 * 1024 * 1024;
        for message in request.messages.iter_mut().rev() {
            if let Message::UserParts { content } = message {
                for part in content.iter_mut().rev() {
                    if let UserContent::Image {
                        path,
                        name,
                        data_url,
                    } = part
                    {
                        if accepts_images && !over_budget {
                            let loaded = store
                                .image_path(&session, path)
                                .map_err(CoreError::from)
                                .and_then(|path| read(&path));
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
