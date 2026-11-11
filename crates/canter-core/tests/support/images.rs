use super::*;
use canter_model::UserContent;
use image::{DynamicImage, GenericImageView, ImageDecoder, ImageEncoder, ImageFormat, RgbImage};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{io::Cursor, path::Path};

fn picture(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let pixels = RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x / 8) as u8, (y / 8) as u8, ((x + y) / 16) as u8])
    });
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(pixels)
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}

fn runtime(root: &Path) -> RuntimeContext {
    RuntimeContext {
        environment: BTreeMap::from([
            ("HOME".into(), root.display().to_string()),
            (
                "CANTER_HOME".into(),
                root.join("state").display().to_string(),
            ),
        ]),
        ..Default::default()
    }
}

fn options() -> CoreOptions {
    CoreOptions {
        sandbox: false,
        prompt_suggestions: false,
        image_models: vec!["fake".into()],
        ..Default::default()
    }
}

fn image_urls(request: &Request) -> Vec<String> {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::UserParts { content } | Message::ToolImages { content } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            UserContent::Image { data_url, .. } => Some(data_url.clone()),
            _ => None,
        })
        .collect()
}

fn decoded_image(url: &str) -> DynamicImage {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bytes = Vec::new();
    let mut bits = 0u32;
    let mut count = 0;
    for byte in url
        .split_once(',')
        .unwrap()
        .1
        .bytes()
        .take_while(|b| *b != b'=')
    {
        bits = (bits << 6) | alphabet.iter().position(|b| *b == byte).unwrap() as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
        }
    }
    image::load_from_memory(&bytes).unwrap()
}

fn legacy_image(store: &Store, root: &Path, bytes: &[u8]) -> String {
    let mut writer = store.create("legacy", settings(root), 0).unwrap();
    let path = store.save_image("legacy", bytes, "png").unwrap();
    let name = path.file_name().unwrap().to_str().unwrap().to_owned();
    writer
        .append_model_history(&serde_json::json!({
            "type": "message", "user_input": true,
            "message": Message::UserParts { content: vec![UserContent::Image {
                path: name.clone(), name: "old.png".into(), data_url: String::new(),
            }] },
        }))
        .unwrap();
    name
}

#[tokio::test]
async fn image_downscale_prepares_legacy_images_once_without_rewriting_history() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("state/sessions")).unwrap());
    let original = picture(ImageFormat::Png, 2048, 1024);
    let original_name = legacy_image(&store, temp.path(), &original);
    let history = store.model_history::<serde_json::Value>("legacy").unwrap()[0].clone();
    let model = ScriptModel::new(
        (0..2)
            .map(|_| vec![ModelEvent::Finished(FinishReason::Stop)])
            .collect(),
    );
    let mut prepared = None;
    for _ in 0..2 {
        let (session, _) = Session::open(
            store.clone(),
            "legacy".into(),
            runtime(temp.path()),
            model.clone(),
            options(),
        )
        .await
        .unwrap();
        let mut events = session
            .subscribe(session.snapshot().await.event_seq)
            .await
            .unwrap();
        session
            .input(input("Continue with the old picture."))
            .await
            .unwrap();
        finished(&mut events).await;
        let mapping = store.prepared_images("legacy");
        let copy = store
            .image_path("legacy", &mapping[&original_name])
            .unwrap();
        let map_path = store
            .session_dir("legacy")
            .unwrap()
            .join("prepared-images.json");
        assert_eq!(
            std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&map_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let current = (
            mapping,
            std::fs::metadata(copy).unwrap().ino(),
            std::fs::metadata(map_path).unwrap().ino(),
        );
        if let Some(previous) = &prepared {
            assert_eq!(&current, previous, "reopening must reuse both files");
        }
        prepared = Some(current);
        session.close().await.unwrap();
    }
    let requests = model.requests.lock().unwrap();
    let urls = image_urls(&requests[0]);
    assert_eq!(urls.len(), 1);
    assert_eq!(decoded_image(&urls[0]).dimensions(), (1568, 784));
    assert_eq!(urls, image_urls(&requests[1]));
    assert_eq!(
        std::fs::read(store.image_path("legacy", &original_name).unwrap()).unwrap(),
        original
    );
    assert_eq!(
        store.model_history::<serde_json::Value>("legacy").unwrap()[0],
        history
    );
}

#[tokio::test]
async fn image_downscale_records_unchanged_legacy_images_as_processed() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("state/sessions")).unwrap());
    let original = picture(ImageFormat::Png, 32, 16);
    let name = legacy_image(&store, temp.path(), &original);
    let model = ScriptModel::new(
        (0..2)
            .map(|_| vec![ModelEvent::Finished(FinishReason::Stop)])
            .collect(),
    );
    let (session, _) = Session::open(
        store.clone(),
        "legacy".into(),
        runtime(temp.path()),
        model.clone(),
        options(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("Small old image.")).await.unwrap();
    finished(&mut events).await;
    assert_eq!(
        store.prepared_images("legacy"),
        BTreeMap::from([(name.clone(), name.clone())])
    );
    let map = store
        .session_dir("legacy")
        .unwrap()
        .join("prepared-images.json");
    let inode = std::fs::metadata(&map).unwrap().ino();
    session.input(input("Continue.")).await.unwrap();
    finished(&mut events).await;
    assert_eq!(std::fs::metadata(map).unwrap().ino(), inode);
    assert_eq!(
        std::fs::read(store.image_path("legacy", &name).unwrap()).unwrap(),
        original
    );
    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(image_urls(&requests[0]), image_urls(&requests[1]));
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn image_downscale_mapping_survives_rewind_retract_and_fork() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("state/sessions")).unwrap());
    let original = picture(ImageFormat::Png, 2048, 1024);
    let original_name = legacy_image(&store, temp.path(), &original);
    let model = ScriptModel::new(vec![
        vec![ModelEvent::Finished(FinishReason::Stop)],
        vec![],
        vec![ModelEvent::Finished(FinishReason::Stop)],
        vec![ModelEvent::Finished(FinishReason::Stop)],
    ]);
    let (session, _) = Session::open(
        store.clone(),
        "legacy".into(),
        runtime(temp.path()),
        model.clone(),
        options(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let turn = session
        .input(input("Before rewinding."))
        .await
        .unwrap()
        .turn_id
        .unwrap();
    finished(&mut events).await;
    let mapping = store.prepared_images("legacy");
    let map_path = store
        .session_dir("legacy")
        .unwrap()
        .join("prepared-images.json");
    let map_inode = std::fs::metadata(&map_path).unwrap().ino();
    let mut rewind: SessionRewindParams = serde_json::from_value(serde_json::json!({
        "beforeTurnId": turn, "rewindFiles": false, "rewindConversation": true,
    }))
    .unwrap();
    rewind.last_event_seq = Some(
        session
            .rewind_preview(rewind.clone())
            .await
            .unwrap()
            .last_event_seq,
    );
    session.rewind(rewind).await.unwrap();
    let turn = session
        .input(input("Retract this."))
        .await
        .unwrap()
        .turn_id
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while model.requests.lock().unwrap().len() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        session
            .interrupt(SessionInterruptParams {
                turn_id: turn,
                retract: Some(true)
            })
            .await
            .unwrap()
            .retracted,
        Some(true)
    );
    // Subscribe after the retraction, whose cancelled turn has no ordinary completion.
    let mut events = session
        .subscribe(session.snapshot().await.event_seq)
        .await
        .unwrap();
    session.input(input("After retracting.")).await.unwrap();
    finished(&mut events).await;
    assert_eq!(std::fs::metadata(&map_path).unwrap().ino(), map_inode);
    assert_eq!(store.prepared_images("legacy"), mapping);
    let (fork, _) = session
        .fork_snapshot()
        .await
        .unwrap()
        .create(
            store.clone(),
            "fork".into(),
            runtime(temp.path()),
            model.clone(),
            options(),
        )
        .await
        .unwrap();
    assert_eq!(store.prepared_images("fork"), mapping);
    for name in [&original_name, &mapping[&original_name]] {
        assert_eq!(
            std::fs::read(store.image_path("legacy", name).unwrap()).unwrap(),
            std::fs::read(store.image_path("fork", name).unwrap()).unwrap()
        );
    }
    let fork_map = store
        .session_dir("fork")
        .unwrap()
        .join("prepared-images.json");
    let fork_inode = std::fs::metadata(&fork_map).unwrap().ino();
    let mut fork_events = fork
        .subscribe(fork.snapshot().await.event_seq)
        .await
        .unwrap();
    fork.input(input("Use the inherited image.")).await.unwrap();
    finished(&mut fork_events).await;
    assert_eq!(std::fs::metadata(fork_map).unwrap().ino(), fork_inode);
    {
        let requests = model.requests.lock().unwrap();
        let expected = image_urls(&requests[0]);
        assert_eq!(decoded_image(&expected[0]).dimensions(), (1568, 784));
        for request in &requests[1..] {
            assert_eq!(image_urls(request), expected);
        }
    }
    fork.close().await.unwrap();
    session.close().await.unwrap();
}

#[tokio::test]
async fn image_downscale_recovers_missing_corrupt_and_unreadable_maps() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("state/sessions")).unwrap());
    let original = picture(ImageFormat::Png, 2048, 1024);
    let name = legacy_image(&store, temp.path(), &original);
    let map = store
        .session_dir("legacy")
        .unwrap()
        .join("prepared-images.json");
    let model = ScriptModel::new(
        (0..5)
            .map(|_| vec![ModelEvent::Finished(FinishReason::Stop)])
            .collect(),
    );
    let (session, _) = Session::open(
        store.clone(),
        "legacy".into(),
        runtime(temp.path()),
        model.clone(),
        options(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    for step in 0..5 {
        match step {
            1 => std::fs::write(&map, b"broken JSON").unwrap(),
            2 => std::fs::remove_file(&map).unwrap(),
            3 => {
                let saved = store.prepared_images("legacy");
                std::fs::remove_file(store.image_path("legacy", &saved[&name]).unwrap()).unwrap();
            }
            4 => {
                std::fs::remove_file(&map).unwrap();
                std::fs::create_dir(&map).unwrap();
            }
            _ => {}
        }
        session.input(input("Use the old picture.")).await.unwrap();
        let seen = finished(&mut events).await;
        assert!(!seen.iter().any(|e| matches!(
            e.payload,
            EventPayload::Error { .. } | EventPayload::Warning { .. }
        )));
        if step < 4 {
            assert!(store.prepared_images("legacy").contains_key(&name));
        }
    }
    {
        let requests = model.requests.lock().unwrap();
        let expected = image_urls(&requests[0]);
        assert_eq!(decoded_image(&expected[0]).dimensions(), (1568, 784));
        for request in &requests[1..] {
            assert_eq!(image_urls(request), expected);
        }
    }
    assert_eq!(
        std::fs::read(store.image_path("legacy", &name).unwrap()).unwrap(),
        original
    );
    session.close().await.unwrap();
}

fn animated_webp() -> Vec<u8> {
    fn chunk(output: &mut Vec<u8>, tag: &[u8; 4], bytes: &[u8]) {
        output.extend(tag);
        output.extend((bytes.len() as u32).to_le_bytes());
        output.extend(bytes);
        if !bytes.len().is_multiple_of(2) {
            output.push(0);
        }
    }
    let frame = picture(ImageFormat::WebP, 1600, 16);
    let mut body = b"WEBP".to_vec();
    chunk(&mut body, b"VP8X", &[2, 0, 0, 0, 63, 6, 0, 15, 0, 0]);
    chunk(&mut body, b"ANIM", &[0; 6]);
    let mut animated = vec![0, 0, 0, 0, 0, 0, 63, 6, 0, 15, 0, 0, 100, 0, 0, 0];
    animated.extend(&frame[12..]);
    chunk(&mut body, b"ANMF", &animated);
    chunk(&mut body, b"ANMF", &animated);
    let mut bytes = b"RIFF".to_vec();
    bytes.extend((body.len() as u32).to_le_bytes());
    bytes.extend(body);
    assert!(
        image::codecs::webp::WebPDecoder::new(Cursor::new(&bytes))
            .unwrap()
            .has_animation()
    );
    bytes
}

#[tokio::test]
async fn image_downscale_preserves_formats_and_keeps_small_animated_failed_or_larger_encodings() {
    let mut gif = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
        for color in [0, 255] {
            encoder
                .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                    1600,
                    16,
                    image::Rgba([color, 0, 0, 255]),
                )))
                .unwrap();
        }
    }
    let mut optimized = Vec::new();
    let pixels = RgbImage::from_fn(1569, 784, |x, y| {
        let level = if (x / 8 + y / 8) % 2 == 0 { 0 } else { 255 };
        image::Rgb([level; 3])
    });
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut optimized, 1)
        .encode_image(&pixels)
        .unwrap();
    let cases = [
        (picture(ImageFormat::Jpeg, 1024, 2048), Some((784, 1568))),
        (picture(ImageFormat::WebP, 2048, 1024), Some((1568, 784))),
        (picture(ImageFormat::Png, 1568, 784), None),
        (picture(ImageFormat::Jpeg, 32, 16), None),
        (b"\x89PNG\r\n\x1a\n".to_vec(), None),
        (gif, None),
        (animated_webp(), None),
        (optimized, None),
    ];
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("state/sessions")).unwrap());
    let model = ScriptModel::new(
        cases
            .iter()
            .map(|_| vec![ModelEvent::Finished(FinishReason::Stop)])
            .collect(),
    );
    let (session, _) = Session::create(
        store.clone(),
        "formats".into(),
        settings(temp.path()),
        runtime(temp.path()),
        model.clone(),
        options(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    for (index, (original, dimensions)) in cases.iter().enumerate() {
        let source = temp.path().join("picture.bin");
        std::fs::write(&source, original).unwrap();
        let mut message = input("Look.");
        message.input.push(UserInput::LocalImage {
            path: source.display().to_string(),
            name: None,
        });
        session.input(message).await.unwrap();
        finished(&mut events).await;
        let snapshot = session.snapshot().await;
        let UserInput::LocalImage { path, .. } = &snapshot.session.turns[index].input[1] else {
            panic!("image");
        };
        let saved = std::fs::read(store.image_path("formats", path).unwrap()).unwrap();
        assert_eq!(
            image::guess_format(&saved).unwrap(),
            image::guess_format(original).unwrap()
        );
        if let Some(dimensions) = dimensions {
            assert_eq!(
                image::load_from_memory(&saved).unwrap().dimensions(),
                *dimensions
            );
            assert!(saved.len() < original.len());
        } else {
            assert!(
                saved == *original,
                "case {index} must keep original bytes ({} versus {} bytes)",
                saved.len(),
                original.len()
            );
        }
    }
    assert!(
        !store
            .session_dir("formats")
            .unwrap()
            .join("prepared-images.json")
            .exists()
    );
    let mut oversized = picture(ImageFormat::Png, 2048, 1024);
    oversized.resize(MAX_IMAGE_BYTES as usize + 1, 0);
    let source = temp.path().join("oversized.png");
    std::fs::write(&source, oversized).unwrap();
    let mut message = input("Too large before resizing.");
    message.input.push(UserInput::LocalImage {
        path: source.display().to_string(),
        name: None,
    });
    assert_eq!(
        session.input(message).await.unwrap_err().message,
        "image exceeds the 8 MiB limit"
    );
    assert_eq!(model.requests.lock().unwrap().len(), cases.len());
    session.close().await.unwrap();
}

#[tokio::test]
async fn image_downscale_saves_a_smaller_copy_and_reuses_its_request_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let pixels = image::load_from_memory(&picture(ImageFormat::Png, 2048, 1024)).unwrap();
    // Opaque synthetic ICC metadata, with the RGB signature used by screenshot profiles.
    let mut profile = vec![0; 128];
    profile[..4].copy_from_slice(&128u32.to_be_bytes());
    profile[16..20].copy_from_slice(b"RGB ");
    profile[36..40].copy_from_slice(b"acsp");
    let mut original = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut original);
    encoder.set_icc_profile(profile.clone()).unwrap();
    pixels.write_with_encoder(encoder).unwrap();
    let source = temp.path().join("source.png");
    std::fs::write(&source, &original).unwrap();
    let store = Arc::new(Store::new(temp.path().join("state/sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![ModelEvent::Finished(FinishReason::Stop)],
        vec![ModelEvent::Finished(FinishReason::Stop)],
    ]);
    let (session, _) = Session::create(
        store.clone(),
        "images".into(),
        settings(temp.path()),
        runtime(temp.path()),
        model.clone(),
        options(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let mut message = input("Look at this.");
    message.input.push(UserInput::LocalImage {
        path: source.display().to_string(),
        name: None,
    });
    session.input(message).await.unwrap();
    finished(&mut events).await;
    let snapshot = session.snapshot().await;
    let UserInput::LocalImage { path, .. } = &snapshot.session.turns[0].input[1] else {
        panic!("saved image reference");
    };
    let saved = std::fs::read(store.image_path("images", path).unwrap()).unwrap();
    assert_eq!(image::guess_format(&saved).unwrap(), ImageFormat::Png);
    assert_eq!(
        image::load_from_memory(&saved).unwrap().dimensions(),
        (1568, 784)
    );
    assert!(saved.len() < original.len());
    let mut decoder = image::ImageReader::new(Cursor::new(&saved))
        .with_guessed_format()
        .unwrap()
        .into_decoder()
        .unwrap();
    assert_eq!(decoder.icc_profile().unwrap(), Some(profile));
    assert_eq!(std::fs::read(&source).unwrap(), original);
    session.input(input("Continue.")).await.unwrap();
    finished(&mut events).await;
    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(image_urls(&requests[0]).len(), 1);
        assert_eq!(image_urls(&requests[0]), image_urls(&requests[1]));
    }
    session.close().await.unwrap();
}
