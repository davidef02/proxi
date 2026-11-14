//! Node path.join for printed paths.

/// `path.join` on the running platform: picks the separator the CLI's stdout uses.
pub fn join(dir: &str, file: &str) -> String {
    if cfg!(windows) {
        join_win32(dir, file)
    } else {
        join_posix(dir, file)
    }
}

pub fn join_posix(dir: &str, file: &str) -> String {
    if dir.is_empty() {
        normalise_posix(file)
    } else {
        normalise_posix(&format!("{dir}/{file}"))
    }
}

pub fn join_win32(dir: &str, file: &str) -> String {
    if dir.is_empty() {
        normalise_win32(file)
    } else {
        normalise_win32(&format!("{dir}\\{file}"))
    }
}

/// Text after the last separator (both separators on Windows, like `path.basename`).
pub fn basename(p: &str) -> &str {
    p.rsplit(|c: char| c == '/' || (cfg!(windows) && c == '\\'))
        .next()
        .unwrap_or(p)
}

fn normalise_posix(input: &str) -> String {
    let absolute = input.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for segment in input.split('/') {
        match segment {
            "" | "." => {}
            ".." => match parts.last() {
                Some(&last) if last != ".." => {
                    parts.pop();
                }
                _ => {
                    if !absolute {
                        parts.push("..");
                    }
                }
            },
            other => parts.push(other),
        }
    }

    let mut result = String::new();
    if absolute {
        result.push('/');
    }
    result.push_str(&parts.join("/"));
    if result.is_empty() {
        result.push('.');
    }
    if input.ends_with('/') && !result.ends_with('/') {
        result.push('/');
    }
    result
}

fn normalise_win32(input: &str) -> String {
    let path = input.replace('/', "\\");

    // Root: UNC share, leading separator, or drive letter. The tail is the rest.
    let (device, absolute, tail) = if path.starts_with("\\\\") && !path[2..].starts_with('\\') {
        let rest = &path[2..];
        let server_end = rest.find('\\').unwrap_or(rest.len());
        let server = &rest[..server_end];
        let after_server = &rest[server_end..];
        let share_end = after_server
            .strip_prefix('\\')
            .map(|s| s.find('\\').map_or(s.len() + 1, |i| i + 1))
            .unwrap_or(0);
        let device = format!("\\\\{server}{}", &after_server[..share_end]);
        (device, true, &after_server[share_end..])
    } else if let Some(rest) = path.strip_prefix('\\') {
        (String::new(), true, rest)
    } else if path.as_bytes().first().is_some_and(|b| b.is_ascii_alphabetic())
        && path.as_bytes().get(1) == Some(&b':')
    {
        // Only ASCII bytes are sliced: the second char may be multi-byte (`là` is not a drive).
        let absolute = path.as_bytes().get(2) == Some(&b'\\');
        (path[..2].to_string(), absolute, &path[2..])
    } else {
        (String::new(), false, &path[..])
    };

    let mut parts: Vec<&str> = Vec::new();
    for segment in tail.split('\\') {
        match segment {
            "" | "." => {}
            ".." => match parts.last() {
                Some(&last) if last != ".." => {
                    parts.pop();
                }
                _ => {
                    if !absolute {
                        parts.push("..");
                    }
                }
            },
            other => parts.push(other),
        }
    }

    let mut result = device;
    if absolute {
        result.push('\\');
    }
    if parts.is_empty() {
        if !absolute {
            result.push('.');
        }
    } else {
        result.push_str(&parts.join("\\"));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values captured from Node's path.win32.join / path.posix.join.
    const VECTORS: &[(&str, &str, &str, &str)] = &[
        (
            "courses/demo/lessons",
            "0001-a.html",
            "courses\\demo\\lessons\\0001-a.html",
            "courses/demo/lessons/0001-a.html",
        ),
        (
            "./courses/demo/lessons/",
            "0001-a.html",
            "courses\\demo\\lessons\\0001-a.html",
            "courses/demo/lessons/0001-a.html",
        ),
        (
            "courses//x/./y/../lessons",
            "0001-a.html",
            "courses\\x\\lessons\\0001-a.html",
            "courses/x/lessons/0001-a.html",
        ),
        (
            "../out",
            "0001-a.html",
            "..\\out\\0001-a.html",
            "../out/0001-a.html",
        ),
        ("../../a/..", "f.html", "..\\..\\f.html", "../../f.html"),
        (".", "f.html", "f.html", "f.html"),
        ("./", "f.html", "f.html", "f.html"),
        (
            "C:/data/lessons",
            "f.html",
            "C:\\data\\lessons\\f.html",
            "C:/data/lessons/f.html",
        ),
        (
            "/abs/data/lessons",
            "f.html",
            "\\abs\\data\\lessons\\f.html",
            "/abs/data/lessons/f.html",
        ),
        (
            "//srv/share/dir",
            "f.html",
            "\\\\srv\\share\\dir\\f.html",
            "/srv/share/dir/f.html",
        ),
        ("/abs/../..", "f.html", "\\f.html", "/f.html"),
        ("C:", "f.html", "C:\\f.html", "C:/f.html"),
        ("a\\b/c", "f.html", "a\\b\\c\\f.html", "a\\b/c/f.html"),
    ];

    #[test]
    fn join_matches_node_vectors() {
        for (dir, file, win32, posix) in VECTORS {
            assert_eq!(
                &join_win32(dir, file),
                win32,
                "win32 join({dir:?}, {file:?})"
            );
            assert_eq!(
                &join_posix(dir, file),
                posix,
                "posix join({dir:?}, {file:?})"
            );
        }
    }

    #[test]
    fn join_uses_the_platform_separator() {
        let expected = if cfg!(windows) {
            "courses\\demo\\lessons\\0001-a.html"
        } else {
            "courses/demo/lessons/0001-a.html"
        };
        assert_eq!(join("courses/demo/lessons", "0001-a.html"), expected);
    }

    #[test]
    fn join_win32_does_not_slice_inside_a_multibyte_char() {
        // The second char is not ASCII: slicing bytes 1..2 would panic before the drive check.
        assert_eq!(join_win32("là", "f"), "là\\f");
    }

    #[test]
    fn basename_takes_the_last_segment() {
        assert_eq!(basename("courses/demo/lessons/0001-a.html"), "0001-a.html");
        assert_eq!(basename("0001-a.html"), "0001-a.html");
        if cfg!(windows) {
            assert_eq!(basename("courses\\demo\\0002-b.html"), "0002-b.html");
        }
    }
}
