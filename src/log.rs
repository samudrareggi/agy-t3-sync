use crate::paths::AppPaths;
use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn set_private_file_perms(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub fn write_private_bytes_atomic(dest: &Path, content: &[u8]) -> io::Result<()> {
    let parent = dest
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    fs::create_dir_all(parent)?;

    let file_name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let tmp = parent.join(format!("{file_name}.agy-t3-sync.tmp"));

    let write_result = (|| -> io::Result<()> {
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(content)?;
        file.sync_all()?;
        Ok(())
    })();

    if let Err(err) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }

    set_private_file_perms(&tmp)?;
    if let Err(err) = fs::rename(&tmp, dest) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
}

fn is_sk_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
}

/// Extract the first non-empty line, redact credential-like substrings, and cap length at 180 chars.
pub fn sanitize_error_line(text: &str) -> String {
    let first = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if first.is_empty() {
        return "unknown error".to_string();
    }

    let bytes = first.as_bytes();
    let mut out = String::with_capacity(first.len());
    let mut i = 0;

    while i < bytes.len() {
        let rest = &first[i..];

        // Redact Bearer <token>
        if bytes.len() - i >= 7 && first[i..i + 7].eq_ignore_ascii_case("bearer ") {
            let mut j = i + 7;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let start_tok = j;
            while j < bytes.len() && is_token_char(bytes[j]) {
                j += 1;
            }
            if j > start_tok {
                out.push_str(&first[i..start_tok]);
                out.push_str("<redacted>");
                i = j;
                continue;
            }
        }

        // Redact ya29.<...> or 1//<...>
        if rest.starts_with("ya29.") || rest.starts_with("1//") {
            let prefix_len = if rest.starts_with("ya29.") { 5 } else { 3 };
            let mut j = i + prefix_len;
            while j < bytes.len() && is_token_char(bytes[j]) {
                j += 1;
            }
            if j > i + prefix_len {
                out.push_str("<redacted>");
                i = j;
                continue;
            }
        }

        // Redact sk-<12+ chars> on word boundary
        let prev_boundary = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if prev_boundary && rest.starts_with("sk-") {
            let mut j = i + 3;
            while j < bytes.len() && is_sk_char(bytes[j]) {
                j += 1;
            }
            if j - (i + 3) >= 12 {
                out.push_str("<redacted>");
                i = j;
                continue;
            }
        }

        let ch = rest.chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }

    out.chars().take(180).collect()
}

pub fn write_sync_log(paths: &AppPaths, msg: &str, quiet: bool) {
    let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("{ts}  {msg}");

    if ensure_private_dir(&paths.base_dir).is_ok() {
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        if let Ok(mut file) = opts.open(&paths.log_path) {
            let _ = set_private_file_perms(&paths.log_path);
            let _ = writeln!(file, "{line}");
        }
    }

    if !quiet {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_redacts_bearer_oauth_and_api_keys() {
        let line = "HTTP 401 Authorization: Bearer secret_token.123 ya29.abc-DEF_9 1//0gXYZ sk-proj1234567890abcd\nsecond line";
        let sanitized = sanitize_error_line(line);
        assert!(!sanitized.contains("secret_token"));
        assert!(!sanitized.contains("ya29."));
        assert!(!sanitized.contains("1//"));
        assert!(!sanitized.contains("sk-proj"));
        assert!(sanitized.contains("<redacted>"));
        assert!(!sanitized.contains("second line"));
    }

    #[test]
    fn atomic_write_creates_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("sub").join("secret.json");
        write_private_bytes_atomic(&dest, b"{\"ok\":true}\n").unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "{\"ok\":true}\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
