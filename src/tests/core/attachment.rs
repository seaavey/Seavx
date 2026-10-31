use super::*;

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([1, 2, 3, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Png)
        .expect("encode");
    bytes.into_inner()
}

fn attachment(bytes: Vec<u8>) -> Attachment {
    let (width, height) = dimensions(&bytes).expect("a png header");
    Attachment::new(
        "image-001.png".to_string(),
        "image/png".to_string(),
        width,
        height,
        bytes,
        PasteSource::Clipboard,
    )
}

#[test]
fn an_attachment_names_itself_from_its_media_type() {
    let mut attached = attachment(png(2, 2));
    assert_eq!(attached.extension(), "png");
    attached.media_type = "image/jpeg".to_string();
    assert_eq!(attached.extension(), "jpeg");
}

#[test]
fn the_live_line_and_the_stored_line_agree() {
    let attached = attachment(png(4, 3));
    let live = attached.summary(1);
    let stored = stored_summary(
        1,
        &attached.name,
        &attached.media_type,
        &base64::engine::general_purpose::STANDARD.encode(&attached.bytes),
    );

    assert_eq!(
        live, stored,
        "a resumed transcript must read the same words"
    );
    assert!(live.starts_with("image #1 4×3 png "), "{live}");
}

#[test]
fn the_same_bytes_hash_the_same() {
    let first = attachment(png(2, 2));
    let second = attachment(png(2, 2));
    let other = attachment(png(3, 3));

    assert_eq!(first.sha256, second.sha256, "dedupe keys on the bytes");
    assert_ne!(first.sha256, other.sha256);
}

#[test]
fn a_size_label_reads_like_a_person() {
    assert_eq!(size_label(512), "512 B");
    assert_eq!(size_label(2048), "2 KiB");
    assert_eq!(size_label(2 * 1024 * 1024), "2.0 MiB");
}

#[test]
fn a_user_message_carries_the_text_and_one_part_per_image() {
    let prompt = "look at this";
    let plain = user_message(prompt, &[]);
    assert_eq!(plain.content.texts(), vec![prompt], "no image, no parts");

    let two = user_message(prompt, &[attachment(png(2, 2)), attachment(png(2, 2))]);
    assert_eq!(two.content.texts(), vec![prompt]);
    assert_eq!(
        two.content
            .parts()
            .iter()
            .filter(|part| matches!(part, ContentPart::Binary(_)))
            .count(),
        2
    );
}

#[test]
fn a_resumed_transcript_shows_the_image_line() {
    let message = user_message("look", &[attachment(png(4, 3))]);
    let rendered = crate::session::render_messages(&[message]);

    assert!(rendered.starts_with("User: look"), "{rendered}");
    assert!(
        rendered.contains("[image #1 4\u{00d7}3 png"),
        "a resumed row must read like the live one: {rendered}"
    );
}

#[test]
fn the_stored_row_round_trips_the_image() {
    let temp = tempfile::tempdir().unwrap();
    let store = crate::session::SessionStore::open_at(&temp.path().join("sessions.db")).unwrap();
    let attached = attachment(png(2, 2));

    let (id, history) = crate::session::prepare_turn(
        &store,
        "ws",
        "m",
        None,
        "look",
        std::slice::from_ref(&attached),
    )
    .unwrap();

    assert!(history.is_empty(), "the unanswered row is the draft");
    let loaded = store.load(id).unwrap();
    let row = loaded.messages.last().expect("the user row");
    let payload: genai::chat::ChatMessage = serde_json::from_str(&row.payload).unwrap();
    let parts = payload.content.parts();
    assert_eq!(parts.len(), 2, "text then binary");
    assert!(
        matches!(parts[1], genai::chat::ContentPart::Binary(_)),
        "the row carries the image, not a summary of it"
    );
}
