//! The routes the webview may reach. Pure. Everything else is refused before the socket.

/// Whether `method path` is one of the routes the screen uses. `path` may have a query.
#[must_use]
pub fn is_allowed(method: &str, path: &str) -> bool {
    if path.bytes().any(|b| b.is_ascii_control() || b == b' ' || b == b'#') {
        return false;
    }
    let route = path.split('?').next().unwrap_or(path);
    let segments: Vec<&str> = route.split('/').collect();
    match (method, segments.as_slice()) {
        ("GET", ["", "v1", "status"] | ["", "v1", "tasks"])
        | ("POST", ["", "v1", "tasks"]) => true,
        ("GET" | "PATCH", ["", "v1", "tasks", id]) => is_id(id),
        ("GET", ["", "v1", "tasks", id, "events"]) | ("POST", ["", "v1", "tasks", id, "undo"]) => {
            is_id(id)
        }
        ("POST", ["", "v1", "tasks", id, "actions", action]) => is_id(id) && is_word(action),
        _ => false,
    }
}

fn is_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 8 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn is_word(word: &str) -> bool {
    !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::is_allowed;

    #[test]
    fn allows_the_routes_of_the_screen() {
        assert!(is_allowed("GET", "/v1/tasks"));
        assert!(is_allowed("GET", "/v1/tasks?context=demo"));
        assert!(is_allowed("POST", "/v1/tasks"));
        assert!(is_allowed("POST", "/v1/tasks/k3q7zm2a/actions/done"));
    }

    #[test]
    fn refuses_everything_else() {
        for (method, path) in [
            ("GET", "/v1/events"),
            ("POST", "/v1/repos/scan"),
            ("POST", "/v1/tasks/sync"),
            ("DELETE", "/v1/repos/a/b"),
            ("GET", "/v2/tasks"),
            ("GET", "/v1/tasks/../repos"),
            ("GET", "/v1/tasks/a%2fb"),
            ("GET", "/v1/tasks\r\nX: y"),
            ("GET", "http://evil/v1/tasks"),
            ("TRACE", "/v1/tasks"),
        ] {
            assert!(!is_allowed(method, path), "{method} {path}");
        }
    }
}
