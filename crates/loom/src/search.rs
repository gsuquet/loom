//! `loom search`: the request and the human output. `search_cmd` does the I/O.

use std::fmt::Write as _;

use loom_api::{ContextName, SearchResult};

use crate::repo::encode_segment;

/// Path of the search endpoint.
pub const SEARCH_PATH: &str = "/v1/search";

/// API path and query of a search for `words` (joined with spaces), in `context` when given,
/// at most `limit` results.
#[must_use]
pub fn search_api_path(words: &[String], context: Option<&ContextName>, limit: usize) -> String {
    let mut path = format!(
        "{SEARCH_PATH}?q={}&limit={limit}",
        encode_segment(&words.join(" "))
    );
    if let Some(context) = context {
        let _ = write!(path, "&context={}", encode_segment(context.as_str()));
    }
    path
}

/// `loom search`: a header, then one row per result, best first. Columns: slug, matched
/// fields (comma-separated), path; aligned, the path last. Nothing when there is no result.
#[must_use]
pub fn render_results(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return String::new();
    }
    let rows: Vec<[String; 3]> =
        std::iter::once(["SLUG".to_owned(), "MATCHED".to_owned(), "PATH".to_owned()])
            .chain(results.iter().map(|result| {
                [
                    result.slug.clone(),
                    result
                        .matched_fields
                        .iter()
                        .map(|field| field.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                    result.path.display().to_string(),
                ]
            }))
            .collect();
    let width = |column: usize| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
    };
    let (w0, w1) = (width(0), width(1));
    rows.iter().fold(String::new(), |mut text, row| {
        let _ = writeln!(text, "{:<w0$}  {:<w1$}  {}", row[0], row[1], row[2]);
        text
    })
}

#[cfg(test)]
mod tests {
    use loom_api::{SearchField, SearchKind, SearchResult};

    use super::{render_results, search_api_path};

    fn result(slug: &str, fields: &[SearchField], path: &str) -> SearchResult {
        SearchResult {
            kind: SearchKind::Repo,
            slug: slug.to_owned(),
            name: slug.rsplit('/').next().unwrap().to_owned(),
            context: slug.split('/').next().unwrap().to_owned(),
            path: path.into(),
            score: 1.0,
            matched_fields: fields.to_vec(),
        }
    }

    #[test]
    fn api_path_encodes_the_query() {
        let words = ["lo".to_owned(), "a&b=c".to_owned(), "é".to_owned()];
        assert_eq!(
            search_api_path(&words, None, 20),
            "/v1/search?q=lo%20a%26b%3Dc%20%C3%A9&limit=20"
        );
        let context = "work".parse().unwrap();
        assert_eq!(
            search_api_path(&["x".to_owned()], Some(&context), 3),
            "/v1/search?q=x&limit=3&context=work"
        );
    }

    #[test]
    fn results_are_aligned_with_the_path_last() {
        let text = render_results(&[
            result(
                "personal/loom",
                &[SearchField::Name, SearchField::Slug, SearchField::Path],
                "/home/me/perso/loom",
            ),
            result("work/api", &[SearchField::Remotes], "/home/me/work/api"),
        ]);
        assert_eq!(
            text,
            "SLUG           MATCHED         PATH\n\
             personal/loom  name,slug,path  /home/me/perso/loom\n\
             work/api       remotes         /home/me/work/api\n"
        );
        assert_eq!(render_results(&[]), "");
    }
}
