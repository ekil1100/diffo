use chrono::{DateTime, SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn hash_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn now_iso() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX);
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn join_args(args: &[String]) -> String {
    if args.is_empty() {
        "working-tree".into()
    } else {
        args.join(" ")
    }
}

// Match the original JSON encoder: each invalid byte becomes one replacement character,
// rather than collapsing an incomplete multi-byte sequence into a single replacement.
pub fn lossy_utf8(mut bytes: &[u8]) -> String {
    let mut output = String::new();
    loop {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                output.push_str(valid);
                return output;
            }
            Err(error) => {
                let valid_end = error.valid_up_to();
                output.push_str(
                    std::str::from_utf8(&bytes[..valid_end]).expect("validated UTF-8 prefix"),
                );
                let invalid_len = error.error_len().unwrap_or(bytes.len() - valid_end);
                for _ in 0..invalid_len {
                    output.push('\u{fffd}');
                }
                bytes = &bytes[valid_end + invalid_len..];
            }
        }
    }
}

pub fn default_author() -> String {
    // Keep the original CLI precedence, including explicitly empty values.
    std::env::var_os("GIT_AUTHOR_NAME")
        .or_else(|| std::env::var_os("USER"))
        .map(|value| lossy_utf8(value.as_encoded_bytes()))
        .unwrap_or_else(|| "local".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector_and_raw_bytes() {
        assert_eq!(
            hash_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(hash_hex(b"\xff"), hash_hex("�".as_bytes()));
    }

    #[test]
    fn invalid_bytes_match_legacy_json_replacement_granularity() {
        assert_eq!(lossy_utf8(b"\xe2\x82"), "��");
        assert_eq!(lossy_utf8(b"\xf0\x90\x80a\xff"), "���a�");
        assert_eq!(lossy_utf8("字🙂".as_bytes()), "字🙂");
    }

    #[test]
    fn normalization_does_not_quote_or_escape_arguments() {
        assert_eq!(join_args(&[]), "working-tree");
        assert_eq!(
            join_args(&["HEAD".into(), "--".into(), "a b".into()]),
            "HEAD -- a b"
        );
    }

    #[test]
    fn timestamp_is_utc_at_second_precision() {
        let timestamp = now_iso();
        assert_eq!(timestamp.len(), 20);
        assert!(timestamp.ends_with('Z'));
        assert!(DateTime::parse_from_rfc3339(&timestamp).is_ok());
    }
}
