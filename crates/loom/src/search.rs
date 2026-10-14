//! `loom search`: the request and the human output. `search_cmd` does the I/O.

use std::fmt::Write as _;

use loom_api::{ContextName, SearchResult};

use crate::repo::encode_segment;

/// Path of the search endpoint.
pub const SEARCH_PATH: &str = "/v1/search";

/// Status column of a task whose `task.md` is invalid.
const INVALID: &str = "invalid";

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

/// Columns of the table, the detail last.
const HEADER: [&str; 6] = ["TYPE", "ITEM", "CONTEXT", "STATUS", "MATCHED", "DETAIL"];

/// The columns of `result`: type, item (slug or task ID), context, status (tasks only),
/// matched fields (comma-separated), detail (path, or title and link keys).
fn row(result: &SearchResult) -> [String; 6] {
    match result {
        SearchResult::Repo(repo) => [
            result.kind().to_string(),
            repo.slug.clone(),
            repo.context.clone(),
            String::new(),
            joined(repo.matched_fields.iter().map(|field| field.as_str())),
            repo.path.display().to_string(),
        ],
        SearchResult::Task(task) => {
            let mut detail = task.title.clone();
            if !task.link_keys.is_empty() {
                if !detail.is_empty() {
                    detail.push(' ');
                }
                let _ = write!(detail, "({})", task.link_keys.join(", "));
            }
            [
                result.kind().to_string(),
                task.id.to_string(),
                task.context.to_string(),
                task.status.clone().unwrap_or_else(|| INVALID.to_owned()),
                joined(task.matched_fields.iter().map(|field| field.as_str())),
                detail,
            ]
        }
    }
}

fn joined<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.collect::<Vec<_>>().join(",")
}

/// `loom search`: a header, then one row per result, best first. Columns: type, item,
/// context, status, matched fields, detail; aligned, the detail last. Nothing when there is no
/// result.
#[must_use]
pub fn render_results(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return String::new();
    }
    let rows: Vec<[String; 6]> = std::iter::once(HEADER.map(str::to_owned))
        .chain(results.iter().map(row))
        .collect();
    let widths: [usize; 6] = std::array::from_fn(|column| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
    });
    rows.iter().fold(String::new(), |mut text, row| {
        let mut line = String::new();
        for (cell, width) in row.iter().zip(widths) {
            let _ = write!(line, "{cell:<width$}  ");
        }
        // An empty detail (an invalid task has no title) leaves no trailing spaces.
        text.push_str(line.trim_end());
        text.push('\n');
        text
    })
}

#[cfg(test)]
mod tests {
    use loom_api::{RepoMatch, SearchField, SearchResult, TaskMatch, TaskSearchField};

    use super::{render_results, search_api_path};

    fn repo(slug: &str, fields: &[SearchField], path: &str) -> SearchResult {
        SearchResult::Repo(RepoMatch {
            slug: slug.to_owned(),
            name: slug.rsplit('/').next().unwrap().to_owned(),
            context: slug.split('/').next().unwrap().to_owned(),
            path: path.into(),
            score: 1.0,
            matched_fields: fields.to_vec(),
        })
    }

    fn task(
        id: &str,
        title: &str,
        status: Option<&str>,
        keys: &[&str],
        fields: &[TaskSearchField],
    ) -> SearchResult {
        SearchResult::Task(TaskMatch {
            id: id.parse().unwrap(),
            context: "work".parse().unwrap(),
            title: title.to_owned(),
            status: status.map(str::to_owned),
            link_keys: keys.iter().map(|key| (*key).to_owned()).collect(),
            score: 1.0,
            matched_fields: fields.to_vec(),
        })
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
    fn results_are_aligned_with_the_detail_last() {
        let text = render_results(&[
            task(
                "k3q7zm2a",
                "Login timeout",
                Some("active"),
                &["CORE-1042", "!88"],
                &[TaskSearchField::Title, TaskSearchField::LinkKeys],
            ),
            repo(
                "personal/loom",
                &[SearchField::Name, SearchField::Slug, SearchField::Path],
                "/home/me/perso/loom",
            ),
            repo("work/api", &[SearchField::Remotes], "/home/me/work/api"),
            task("zzzzzzzz", "", None, &[], &[TaskSearchField::Notes]),
            task(
                "aaaaaaaa",
                "Old",
                Some("done"),
                &[],
                &[TaskSearchField::Title],
            ),
        ]);
        assert_eq!(
            text,
            "TYPE  ITEM           CONTEXT   STATUS   MATCHED          DETAIL\n\
             task  k3q7zm2a       work      active   title,link_keys  Login timeout (CORE-1042, !88)\n\
             repo  personal/loom  personal           name,slug,path   /home/me/perso/loom\n\
             repo  work/api       work               remotes          /home/me/work/api\n\
             task  zzzzzzzz       work      invalid  notes\n\
             task  aaaaaaaa       work      done     title            Old\n"
        );
        assert_eq!(render_results(&[]), "");
    }
}
