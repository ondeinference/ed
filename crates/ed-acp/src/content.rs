//! Converting ACP prompt content into model content: text, embedded and linked files
//! (inlined, a selection as just its lines), images, and audio saved to a scratch file.

use std::path::{Path, PathBuf};

use agent_client_protocol::schema::v1::{
    ClientCapabilities, ContentBlock, EmbeddedResource, EmbeddedResourceResource,
    ReadTextFileRequest, ResourceLink, SessionId, TextResourceContents,
};
use agent_client_protocol::{Client, ConnectionTo};
use anyhow::{Context, Result};
use base64::Engine;
use serde_json::{Value, json};

fn embedded_text(res: &EmbeddedResource) -> Option<String> {
    match &res.resource {
        EmbeddedResourceResource::TextResourceContents(r) => {
            Some(format!("<file uri=\"{}\">\n{}\n</file>", r.uri, r.text))
        }
        // Describe binary resources rather than dropping them, so the model knows they exist.
        EmbeddedResourceResource::BlobResourceContents(b) => Some(format!(
            "[Attached binary resource: {} ({})]",
            b.uri,
            b.mime_type.as_deref().unwrap_or("unknown type")
        )),
        _ => None,
    }
}

/// How audio reaches the model. A product with an audio tool points the model at it here.
#[derive(Clone, Copy)]
pub struct AudioHints {
    /// The text for a `file://` link to an audio file, given its path.
    pub link: fn(&Path) -> String,
    /// The text for an attached clip, given its MIME type and the path it was saved to.
    pub attachment: fn(&str, &Path) -> String,
}

impl Default for AudioHints {
    fn default() -> Self {
        Self {
            link: |path| format!("[Referenced audio file: {}]", path.display()),
            attachment: |mime, path| {
                format!("[Attached audio ({mime}) saved at {}]", path.display())
            },
        }
    }
}

/// How a resource link is shown to the model. Audio files are never read as text; they get
/// the product's [`AudioHints::link`] text instead.
fn link_text(link: &ResourceLink, hints: &AudioHints) -> String {
    match parse_file_link(&link.uri) {
        Some(FileLink { path, .. }) if is_audio(&path, link.mime_type.as_deref()) => {
            (hints.link)(&path)
        }
        _ => format!("[Referenced: {}]", link.uri),
    }
}

/// Common audio file extensions; a link to one is never read as text.
const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "wave", "mp3", "flac", "m4a", "aac", "mp4", "ogg", "oga", "opus", "aif", "aiff", "aifc",
    "caf", "webm", "mkv",
];

fn is_audio(path: &Path, mime: Option<&str>) -> bool {
    mime.is_some_and(|m| m.starts_with("audio/"))
        || path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Largest file a `file://` resource link is inlined for; bigger ones stay as references.
const MAX_LINKED_FILE_BYTES: usize = 256 * 1024;

/// Replace `file://` resource links to text files with their embedded contents, so the model
/// sees the file. Reads through the client when it supports `fs/read_text_file` (unsaved
/// buffers), else from disk. A link to a selection (`#L10:20`) is inlined as just those
/// lines. Audio files, links that can't be read and oversized files stay as references.
pub async fn resolve_resource_links(
    blocks: &[ContentBlock],
    caps: &ClientCapabilities,
    connection: &ConnectionTo<Client>,
    session_id: &SessionId,
) -> Vec<ContentBlock> {
    let mut out = Vec::with_capacity(blocks.len());
    for block in blocks {
        if let ContentBlock::ResourceLink(link) = block
            && let Some(FileLink { path, lines }) = parse_file_link(&link.uri)
            && !is_audio(&path, link.mime_type.as_deref())
        {
            let text = if caps.fs.read_text_file {
                connection
                    .send_request(ReadTextFileRequest::new(session_id.clone(), path.clone()))
                    .block_task()
                    .await
                    .map(|r| r.content)
                    .map_err(|e| e.to_string())
            } else {
                tokio::fs::read_to_string(&path)
                    .await
                    .map_err(|e| e.to_string())
            };
            let text = match lines {
                Some(range) => text.map(|t| slice_lines(&t, range)),
                None => text,
            };
            match text {
                Ok(text) if lines.is_some() && text.is_empty() => {
                    tracing::debug!("{} selects no lines", link.uri)
                }
                Ok(text) if text.len() <= MAX_LINKED_FILE_BYTES => {
                    out.push(ContentBlock::Resource(EmbeddedResource::new(
                        EmbeddedResourceResource::TextResourceContents(
                            TextResourceContents::new(text, link.uri.clone())
                                .mime_type(link.mime_type.clone()),
                        ),
                    )));
                    continue;
                }
                Ok(_) => tracing::debug!("{} too large to inline", link.uri),
                Err(e) => tracing::debug!("could not read {}: {e}", link.uri),
            }
        }
        out.push(block.clone());
    }
    out
}

/// A `file://` resource link: the absolute path, and the lines it selects, if any.
#[derive(Debug, PartialEq)]
struct FileLink {
    path: PathBuf,
    /// 1-based, inclusive.
    lines: Option<(usize, usize)>,
}

/// Parse a `file://` URI, percent-decoding the path. Editors put more than the path in it:
/// Zed links a selection as `file:///a.md?column=5#L10:20` and a symbol as
/// `file:///a.rs?symbol=main#L3:9`. The query is dropped and the fragment read as a line
/// range; a literal `?` or `#` in a file name arrives percent-encoded, so splitting first is
/// safe.
fn parse_file_link(uri: &str) -> Option<FileLink> {
    let rest = uri.strip_prefix("file://")?;
    let (rest, fragment) = match rest.split_once('#') {
        Some((rest, fragment)) => (rest, Some(fragment)),
        None => (rest, None),
    };
    let rest = rest.split_once('?').map_or(rest, |(rest, _)| rest);
    // Allow an authority of "" or "localhost"; reject other hosts.
    let path = if rest.starts_with('/') {
        rest
    } else {
        rest.strip_prefix("localhost")?
    };
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
        {
            decoded.push(b);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    let path = PathBuf::from(String::from_utf8(decoded).ok()?);
    path.is_absolute().then(|| FileLink {
        path,
        lines: fragment.and_then(parse_line_range),
    })
}

/// A `#L10:20` fragment as a 1-based inclusive range. Accepts `L10:20`, `L10-20`, `L10-L20`
/// and a single line `L10`.
fn parse_line_range(fragment: &str) -> Option<(usize, usize)> {
    let range = fragment.strip_prefix('L')?;
    let (start, end) = range
        .split_once(':')
        .or_else(|| range.split_once('-'))
        .unwrap_or((range, range));
    let end = end.strip_prefix('L').unwrap_or(end);
    let (start, end) = (start.parse::<usize>().ok()?, end.parse::<usize>().ok()?);
    (start >= 1 && end >= start).then_some((start, end))
}

/// Lines `start..=end` (1-based) of `text`, keeping their line endings.
fn slice_lines(text: &str, (start, end): (usize, usize)) -> String {
    text.split_inclusive('\n')
        .skip(start - 1)
        .take(end - start + 1)
        .collect()
}

/// Flatten text-like prompt blocks into one string (used for titles and slash commands).
pub fn prompt_to_text(blocks: &[ContentBlock], hints: &AudioHints) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(t) => parts.push(t.text.clone()),
            ContentBlock::ResourceLink(link) => parts.push(link_text(link, hints)),
            ContentBlock::Resource(res) => parts.extend(embedded_text(res)),
            _ => {}
        }
    }
    parts.join("\n\n")
}

fn audio_extension(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or("").trim() {
        "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => "wav",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/ogg" | "audio/vorbis" | "application/ogg" => "ogg",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac" => "m4a",
        "audio/aiff" | "audio/x-aiff" => "aiff",
        "audio/webm" | "video/webm" => "webm",
        _ => "audio",
    }
}

/// Write each audio block under `dir`; returns the saved path per audio block, in order.
pub async fn save_audio_blocks(blocks: &[ContentBlock], dir: &Path) -> Result<Vec<PathBuf>> {
    let mut saved = Vec::new();
    for block in blocks {
        let ContentBlock::Audio(audio) = block else {
            continue;
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(audio.data.trim())
            .context("audio block is not valid base64")?;
        tokio::fs::create_dir_all(dir).await?;
        let path = dir.join(format!(
            "audio-{}.{}",
            uuid::Uuid::new_v4().simple(),
            audio_extension(&audio.mime_type)
        ));
        tokio::fs::write(&path, bytes).await?;
        saved.push(path);
    }
    Ok(saved)
}

/// Convert prompt blocks to an OpenAI `content` value: a string for text only, or parts when
/// images are present. Audio is referenced by the path it was saved to, worded by
/// [`AudioHints::attachment`]. `slash` replaces the first text block (an expanded slash
/// command).
pub fn prompt_to_content(
    blocks: &[ContentBlock],
    audio_paths: &[PathBuf],
    slash: Option<&str>,
    hints: &AudioHints,
) -> Value {
    let mut audio = audio_paths.iter();
    let mut parts: Vec<Value> = Vec::new();
    let mut slash = slash;
    let mut has_image = false;
    let text = |t: String| json!({ "type": "text", "text": t });
    for block in blocks {
        match block {
            ContentBlock::Text(t) => match slash.take() {
                Some(expanded) => parts.push(text(expanded.to_string())),
                None => parts.push(text(t.text.clone())),
            },
            ContentBlock::ResourceLink(link) => parts.push(text(link_text(link, hints))),
            ContentBlock::Resource(res) => {
                if let Some(t) = embedded_text(res) {
                    parts.push(text(t));
                }
            }
            ContentBlock::Image(img) => {
                has_image = true;
                let url = format!("data:{};base64,{}", img.mime_type, img.data);
                parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
            }
            ContentBlock::Audio(a) => match audio.next() {
                Some(path) => parts.push(text((hints.attachment)(&a.mime_type, path))),
                None => parts.push(text("[Attached audio could not be saved]".into())),
            },
            _ => {}
        }
    }
    if has_image {
        return Value::Array(parts);
    }
    let joined = parts
        .iter()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n\n");
    Value::String(joined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{AudioContent, ImageContent, TextContent};

    fn link(uri: &str) -> Option<FileLink> {
        parse_file_link(uri)
    }

    #[test]
    fn plain_paths_decode_and_must_be_absolute() {
        assert_eq!(
            link("file:///a%20b/c.md"),
            Some(FileLink {
                path: "/a b/c.md".into(),
                lines: None
            })
        );
        assert_eq!(
            link("file://localhost/x.md").unwrap().path,
            Path::new("/x.md")
        );
        assert_eq!(link("file://other-host/x.md"), None);
        assert_eq!(link("https://example.com/x.md"), None);
    }

    #[test]
    fn zed_selection_and_symbol_links() {
        let sel = link("file:///notes/song.md?column=5#L10:20").unwrap();
        assert_eq!(sel.path, Path::new("/notes/song.md"));
        assert_eq!(sel.lines, Some((10, 20)));
        assert_eq!(
            link("file:///a.rs?symbol=main#L3:9").unwrap().lines,
            Some((3, 9))
        );
    }

    #[test]
    fn line_range_forms() {
        assert_eq!(parse_line_range("L10:20"), Some((10, 20)));
        assert_eq!(parse_line_range("L10-20"), Some((10, 20)));
        assert_eq!(parse_line_range("L10-L20"), Some((10, 20)));
        assert_eq!(parse_line_range("L7"), Some((7, 7)));
        assert_eq!(parse_line_range("L0"), None);
        assert_eq!(parse_line_range("L5:2"), None);
        assert_eq!(parse_line_range("x"), None);
    }

    #[test]
    fn slices_inclusive_lines() {
        assert_eq!(slice_lines("a\nb\nc\nd\n", (2, 3)), "b\nc\n");
        assert_eq!(slice_lines("a\nb", (2, 9)), "b");
        assert_eq!(slice_lines("a\n", (5, 6)), "");
    }

    #[test]
    fn audio_links_use_the_product_hint_and_are_never_read() {
        let hints = AudioHints {
            link: |p| format!("analyze {}", p.display()),
            ..AudioHints::default()
        };
        let wav = ResourceLink::new("mix.wav", "file:///music/mix%20v2.WAV");
        assert_eq!(link_text(&wav, &hints), "analyze /music/mix v2.WAV");
        let by_mime =
            ResourceLink::new("take", "file:///t/take").mime_type("audio/flac".to_string());
        assert_eq!(
            link_text(&by_mime, &AudioHints::default()),
            "[Referenced audio file: /t/take]"
        );
        let md = ResourceLink::new("notes", "file:///t/notes.md");
        assert_eq!(link_text(&md, &hints), "[Referenced: file:///t/notes.md]");
    }

    #[tokio::test]
    async fn audio_is_saved_and_referenced() {
        let dir = std::env::temp_dir().join(format!("sf-prompt-{}", uuid::Uuid::new_v4()));
        let data = base64::engine::general_purpose::STANDARD.encode(b"RIFFfake");
        let blocks = vec![
            ContentBlock::Text(TextContent::new("what key?")),
            ContentBlock::Audio(AudioContent::new(data, "audio/wav")),
        ];
        let saved = save_audio_blocks(&blocks, &dir).await.unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].extension().unwrap(), "wav");
        assert_eq!(std::fs::read(&saved[0]).unwrap(), b"RIFFfake");
        let content = prompt_to_content(&blocks, &saved, None, &AudioHints::default());
        let s = content.as_str().unwrap();
        assert!(s.contains("what key?") && s.contains(&saved[0].display().to_string()));
        std::fs::remove_dir_all(dir).ok();

        let bad = vec![ContentBlock::Audio(AudioContent::new("!!!", "audio/wav"))];
        assert!(
            save_audio_blocks(&bad, &std::env::temp_dir())
                .await
                .is_err()
        );
    }

    #[test]
    fn images_use_parts_and_slash_replaces_text() {
        let blocks = vec![
            ContentBlock::Text(TextContent::new("/chords Am F")),
            ContentBlock::Image(ImageContent::new("AAAA", "image/png")),
        ];
        let v = prompt_to_content(&blocks, &[], Some("EXPANDED"), &AudioHints::default());
        let parts = v.as_array().unwrap();
        assert_eq!(parts[0]["text"], "EXPANDED");
        assert_eq!(parts[1]["type"], "image_url");
        let plain = prompt_to_content(&blocks[..1], &[], None, &AudioHints::default());
        assert_eq!(plain, json!("/chords Am F"));
    }
}
