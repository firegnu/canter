use std::{fs::File, io::Read, path::Path, sync::Arc};

use canter_model::{Message, Request, UserContent};
use canter_protocol::{ErrorCode, MAX_IMAGE_BYTES, UserInput, image_mime_type};
use canter_store::Store;

use crate::CoreError;

fn read(path: &Path) -> Result<(Vec<u8>, &'static str), CoreError> {
    let invalid = |message| CoreError::new(ErrorCode::InvalidRequest, message);
    if !path.is_file() {
        return Err(invalid("cannot read image file (expected a regular file)"));
    }
    let file = File::open(path).map_err(|_| invalid("cannot read image file"))?;
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

pub(crate) async fn import(
    store: Arc<Store>,
    session: String,
    cwd: String,
    mut input: Vec<UserInput>,
) -> Result<Vec<UserInput>, CoreError> {
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
                Some(read(&path).map(|(bytes, mime)| {
                    (
                        index,
                        bytes,
                        mime,
                        name.clone().unwrap_or_else(|| {
                            path.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned()
                        }),
                    )
                }))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut saved = Vec::new();
        for (index, bytes, mime, name) in images {
            match store.save_image(&session, &bytes, mime.strip_prefix("image/").unwrap()) {
                Ok(path) => {
                    input[index] = UserInput::LocalImage {
                        path: path.to_string_lossy().into_owned(),
                        name: Some(name),
                    };
                    saved.push(path);
                }
                Err(error) => {
                    for path in saved {
                        let _ = std::fs::remove_file(path);
                    }
                    return Err(CoreError::from(error));
                }
            }
        }
        Ok(input)
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

pub(crate) async fn prepare(
    mut request: Request,
    accepts_images: bool,
) -> Result<Request, CoreError> {
    if !request
        .messages
        .iter()
        .any(|message| matches!(message, Message::UserParts { .. }))
    {
        return Ok(request);
    }
    tokio::task::spawn_blocking(move || {
        for message in &mut request.messages {
            if let Message::UserParts { content } = message {
                for part in content {
                    if let UserContent::Image {
                        path,
                        name,
                        data_url,
                    } = part
                    {
                        if accepts_images {
                            let (bytes, mime) = read(Path::new(path))?;
                            *data_url = format!("data:{mime};base64,{}", base64(&bytes));
                        } else {
                            *part = UserContent::Text {
                                text: format!("[image omitted: {name}]"),
                            };
                        }
                    }
                }
            }
        }
        Ok(request)
    })
    .await
    .map_err(|_| CoreError::internal())?
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
