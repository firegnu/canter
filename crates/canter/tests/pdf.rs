use canter_protocol::PermissionMode;
use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, Tool, ToolOutcome, Tools};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

// Synthetic PDF, no user data or external fixture generator.
fn pdf(pages: &[&str]) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!(
            "<< /Type /Pages /Count {} /Kids [{}] >>",
            pages.len(),
            (0..pages.len())
                .map(|i| format!("{} 0 R", 4 + i * 2))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    for (i, text) in pages.iter().enumerate() {
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>", 5 + i * 2));
        let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));
    }
    encode_pdf(&objects)
}

fn encode_pdf(objects: &[String]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0];
    for (i, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in &offsets[1..] {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    bytes
}

fn tools(dir: &tempfile::TempDir) -> Tools {
    Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ))
    .with_pdf_commands(env!("CARGO_BIN_EXE_canter").into(), "pdftoppm".into())
}

async fn read(tools: &Tools, args: serde_json::Value) -> canter_tools::ToolOutput {
    read_with_images(tools, args, true).await
}

async fn read_with_images(
    tools: &Tools,
    args: serde_json::Value,
    images: bool,
) -> canter_tools::ToolOutput {
    let cancel = tokio_util::sync::CancellationToken::new();
    read_with_cancel(tools, args, images, &cancel).await
}

async fn read_with_cancel(
    tools: &Tools,
    args: serde_json::Value,
    images: bool,
    cancel: &tokio_util::sync::CancellationToken,
) -> canter_tools::ToolOutput {
    let call = match tools
        .prepare(Tool::Read, "pdf", &args.to_string(), cancel)
        .await
    {
        Ok(call) => call,
        Err(error) => return error,
    };
    let PermissionCheck::Execute(permit) =
        PermissionGate::default().evaluate(&call, PermissionMode::AllowAll)
    else {
        panic!("permit")
    };
    tools
        .execute_with_images(permit, cancel, &mut |_| {}, images)
        .await
}

#[tokio::test]
async fn pdf_scanned_pages_are_listed_with_model_appropriate_guidance() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("scan.pdf"), pdf(&["", "", ""])).unwrap();
    for images in [true, false] {
        let output = read_with_images(
            &tools(&dir),
            json!({"path":"scan.pdf","lineCount":1}),
            images,
        )
        .await;
        assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
        assert!(
            output
                .text
                .contains("Pages with no extractable text (OCR needed): 1, 2, 3."),
            "{}",
            output.text
        );
        assert_eq!(output.text.contains("page"), images, "{}", output.text);
        assert!(output.images.is_empty());
    }
}

#[tokio::test]
async fn pdf_text_has_page_numbers_and_is_detected_by_content() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("document.bin"),
        pdf(&[
            "First page contains readable text.",
            "Second page contains different text.",
        ]),
    )
    .unwrap();
    let output = read(&tools(&dir), json!({"path":"document.bin"})).await;
    assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
    assert!(output.text.contains("Page 1"), "{}", output.text);
    assert!(output.text.contains("Page 2"), "{}", output.text);
    assert!(
        output.text.contains("First page contains readable text."),
        "{}",
        output.text
    );
    assert!(
        output.text.contains("Second page contains different text."),
        "{}",
        output.text
    );
    assert!(output.images.is_empty());
}

#[tokio::test]
async fn pdf_page_image_uses_the_existing_tool_image_type() {
    if std::process::Command::new("pdftoppm")
        .arg("-v")
        .output()
        .is_err()
    {
        eprintln!("Skipping PDF rendering: pdftoppm is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("chart.pdf"),
        pdf(&["Page one.", "Page two."]),
    )
    .unwrap();
    let output = read(&tools(&dir), json!({"path":"chart.pdf","page":2})).await;
    assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
    assert_eq!(output.images.len(), 1);
    let image = &output.images[0];
    assert_eq!(image.mime_type, "image/png");
    assert_eq!(image.name, "chart.pdf-page2.png");
    assert!(image.bytes.len() as u64 <= canter_protocol::MAX_IMAGE_BYTES);
    assert_eq!(image.bytes.get(..8), Some(b"\x89PNG\r\n\x1a\n".as_slice()));
}

#[tokio::test]
async fn pdf_page_image_without_image_support_does_not_start_a_converter() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("chart.pdf"), pdf(&["Text."])).unwrap();
    let tools = tools(&dir).with_pdf_commands(
        dir.path().join("missing-worker"),
        dir.path().join("missing-renderer"),
    );
    let output = read_with_images(&tools, json!({"path":"chart.pdf","page":1}), false).await;
    assert!(
        output
            .text
            .contains("current model does not support images"),
        "{}",
        output.text
    );
    assert!(output.images.is_empty());
}

#[tokio::test]
async fn pdf_missing_poppler_only_prevents_images() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("chart.pdf"), pdf(&["Readable text."])).unwrap();
    let tools = tools(&dir).with_pdf_commands(
        env!("CARGO_BIN_EXE_canter").into(),
        dir.path().join("missing-renderer"),
    );
    let output = read(&tools, json!({"path":"chart.pdf","page":1})).await;
    assert_eq!(output.outcome, ToolOutcome::Failed);
    assert!(
        output.text.contains("brew install poppler"),
        "{}",
        output.text
    );
    let output = read(&tools, json!({"path":"chart.pdf"})).await;
    assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
    assert!(output.text.contains("Readable text."));
}

#[tokio::test]
async fn pdf_page_parameter_rejects_out_of_range_and_non_pdf() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("chart.pdf"), pdf(&["Text."])).unwrap();
    std::fs::write(dir.path().join("plain.txt"), "ordinary text").unwrap();
    for (path, page, expected) in [
        ("chart.pdf", 0, "page must be positive"),
        ("chart.pdf", 2, "out of range"),
        ("plain.txt", 1, "page is only valid for PDF"),
    ] {
        let output = read(&tools(&dir), json!({"path":path,"page":page})).await;
        assert_eq!(output.outcome, ToolOutcome::Failed);
        assert!(output.text.contains(expected), "{}", output.text);
    }
}

#[tokio::test]
async fn pdf_line_pagination_continues_without_losing_text() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pages.pdf"),
        pdf(&["First paragraph.", "Second paragraph.", "Third paragraph."]),
    )
    .unwrap();
    let tools = tools(&dir);
    let first = read(&tools, json!({"path":"pages.pdf","lineCount":3})).await;
    assert!(
        first.text.contains("More content: read with startLine=4."),
        "{}",
        first.text
    );
    let rest = read(&tools, json!({"path":"pages.pdf","startLine":4})).await;
    let all = read(&tools, json!({"path":"pages.pdf"})).await;
    let numbered = |text: &str| {
        text.lines()
            .filter(|line| {
                line.split_once(": ")
                    .is_some_and(|(n, _)| n.parse::<u32>().is_ok())
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        format!("{}\n{}", numbered(&first.text), numbered(&rest.text)),
        numbered(&all.text)
    );
    assert!(all.text.contains("Third paragraph."));
}

#[tokio::test]
async fn pdf_chinese_text_is_extracted_correctly() {
    let cmap = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def /CMapName /Test def /CMapType 2 def 1 begincodespacerange <0000> <FFFF> endcodespacerange 4 beginbfchar <0001> <4E2D> <0002> <6587> <0003> <6D4B> <0004> <8BD5> endbfchar endcmap CMapName currentdict /CMap defineresource pop end end";
    let content = "BT /F1 12 Tf 72 720 Td <0001000200030004> Tj ET";
    let bytes = encode_pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Count 1 /Kids [4 0 R] >>".into(),
        "<< /Type /Font /Subtype /Type0 /BaseFont /TestFont /Encoding /Identity-H /DescendantFonts [6 0 R] /ToUnicode 7 0 R >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R >>".into(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /TestFont /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /DW 1000 >>".into(),
        format!("<< /Length {} >>\nstream\n{cmap}\nendstream", cmap.len()),
    ]);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("chinese.pdf"), bytes).unwrap();
    let output = read(&tools(&dir), json!({"path":"chinese.pdf"})).await;
    assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
    assert!(output.text.contains("中文测试"), "{}", output.text);
}

#[tokio::test]
async fn pdf_over_50_mib_is_rejected_before_conversion() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let mut file = std::fs::File::create(dir.path().join("large.pdf")).unwrap();
    file.write_all(b"%PDF-1.4\n").unwrap();
    file.set_len(50 * 1024 * 1024 + 1).unwrap();
    let tools =
        tools(&dir).with_pdf_commands(dir.path().join("missing"), dir.path().join("missing"));
    for args in [
        json!({"path":"large.pdf"}),
        json!({"path":"large.pdf","page":1}),
    ] {
        let output = read(&tools, args).await;
        assert_eq!(output.outcome, ToolOutcome::Failed);
        assert!(
            output.text.contains("PDF exceeds the 50 MiB limit"),
            "{}",
            output.text
        );
    }
}

#[tokio::test]
async fn pdf_damage_fails_only_this_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("broken.pdf"), b"%PDF-1.7\ninvalid PDF\x00").unwrap();
    std::fs::write(dir.path().join("plain.txt"), "still working\n").unwrap();
    let tools = tools(&dir);
    let output = read(&tools, json!({"path":"broken.pdf"})).await;
    assert_eq!(output.outcome, ToolOutcome::Failed);
    assert!(
        output.text.contains("encrypted or damaged"),
        "{}",
        output.text
    );
    let output = read(&tools, json!({"path":"plain.txt"})).await;
    assert_eq!(output.outcome, ToolOutcome::Completed);
    assert_eq!(output.text, "1: still working\n");
}

#[tokio::test]
async fn pdf_text_and_images_obey_file_rules_and_protected_state_paths() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("state")).unwrap();
    let bytes = pdf(&["Only synthetic data."]);
    std::fs::write(dir.path().join("state/private.pdf"), &bytes).unwrap();
    std::fs::write(dir.path().join("restricted.pdf"), &bytes).unwrap();
    let rules = dir.path().join("rules.json");
    std::fs::write(
        &rules,
        r#"{"rules":[],"fileRules":[{"pattern":"restricted.pdf","decision":"forbidden"}]}"#,
    )
    .unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(
            dir.path().into(),
            BTreeMap::new(),
            vec![dir.path().join("state")],
        )
        .unwrap(),
    ));
    let gate = PermissionGate::default().with_rules_file(rules);
    for page in [None, Some(1)] {
        let mut args = json!({"path":"restricted.pdf"});
        if let Some(page) = page {
            args["page"] = json!(page);
        }
        let call = tools
            .prepare(
                Tool::Read,
                "pdf",
                &args.to_string(),
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        for mode in [
            PermissionMode::AllowAll,
            PermissionMode::AskForExecution,
            PermissionMode::AlwaysAsk,
        ] {
            assert!(matches!(
                gate.evaluate(&call, mode),
                PermissionCheck::Forbidden { .. }
            ));
        }
        args["path"] = json!("state/private.pdf");
        let output = read(&tools, args).await;
        assert_eq!(output.outcome, ToolOutcome::Failed);
        assert_eq!(output.text, "this file is private to canter");
    }
}

fn waiting_converter(dir: &tempfile::TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let command = dir.path().join("converter");
    let pid = dir.path().join("converter.pid");
    std::fs::write(
        &command,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec /bin/sleep 60\n",
            pid.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).unwrap();
    (command, pid)
}

async fn wait_for_pid(path: &std::path::Path) -> i32 {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(path)
                && let Ok(pid) = pid.parse()
            {
                return pid;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("converter did not start")
}

async fn assert_process_stopped(pid: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None)
                == Err(nix::errno::Errno::ESRCH)
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("PDF converter survived read completion");
}

#[tokio::test]
async fn pdf_timeout_stops_text_and_image_converters() {
    for image in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.pdf"), pdf(&["Readable text."])).unwrap();
        let (command, pid_file) = waiting_converter(&dir);
        let worker = if image {
            env!("CARGO_BIN_EXE_canter").into()
        } else {
            command.clone()
        };
        let tools = tools(&dir)
            .with_pdf_commands(worker, command)
            .with_pdf_timeout(std::time::Duration::from_secs(5));
        let mut args = json!({"path":"test.pdf"});
        if image {
            args["page"] = json!(1);
        }
        let output = read(&tools, args).await;
        assert_eq!(output.outcome, ToolOutcome::TimedOut, "{}", output.text);
        assert!(output.text.contains("PDF conversion timed out"));
        assert_process_stopped(wait_for_pid(&pid_file).await).await;
    }
}

#[tokio::test]
async fn pdf_cancellation_stops_text_and_image_converters() {
    for image in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.pdf"), pdf(&["Readable text."])).unwrap();
        let (command, pid_file) = waiting_converter(&dir);
        let worker = if image {
            env!("CARGO_BIN_EXE_canter").into()
        } else {
            command.clone()
        };
        let tools = tools(&dir).with_pdf_commands(worker, command);
        let mut args = json!({"path":"test.pdf"});
        if image {
            args["page"] = json!(1);
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let (output, pid) = tokio::join!(read_with_cancel(&tools, args, true, &cancel), async {
            let pid = wait_for_pid(&pid_file).await;
            cancel.cancel();
            pid
        });
        assert_eq!(output.outcome, ToolOutcome::Cancelled, "{}", output.text);
        assert_process_stopped(pid).await;
    }
}
