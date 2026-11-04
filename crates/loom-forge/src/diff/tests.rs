use super::{file_hashes, reduced_patch, reduced_patch_within, since_last_review};
use crate::model::{FileChange, FileDiff};

const JWT_V1: &str = "\
diff --git a/src/auth/jwt.rs b/src/auth/jwt.rs
index 1111111..2222222 100644
--- a/src/auth/jwt.rs
+++ b/src/auth/jwt.rs
@@ -7,7 +7,10 @@ pub struct Claims {
 
 pub fn verify(token: &str) -> Result<Claims, Error> {
     let claims = decode(token)?;
-    if claims.exp < now() {
+    if claims.exp <= now() {
         return Err(Error::Expired);
     }
     Ok(claims)
";

fn file(path: &str, change: FileChange, body: Option<&str>) -> FileDiff {
    FileDiff {
        path: path.to_owned(),
        old_path: None,
        change,
        patch: body.map(str::to_owned),
        too_large: false,
        generated: false,
    }
}

fn hash_of(text: &str, path: &str) -> String {
    file_hashes(text.as_bytes())
        .into_iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("no {path} in {text}"))
        .hash
        .unwrap_or_else(|| panic!("no hash of {path} in {text}"))
}

#[test]
fn each_file_of_a_patch_has_its_hash_in_order() {
    let patch = format!(
        "{JWT_V1}diff --git a/x.rs b/x.rs\nindex 3..4 100644\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-a\n+b\n"
    );
    let files = file_hashes(patch.as_bytes());
    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["src/auth/jwt.rs", "x.rs"]);
    assert_eq!(files[0].hash.as_ref().unwrap().len(), 64);
    assert_ne!(files[0].hash, files[1].hash);
}

#[test]
fn index_lines_line_numbers_and_section_text_do_not_change_the_hash() {
    let moved = JWT_V1
        .replace(
            "index 1111111..2222222 100644",
            "index aaaaaaa..bbbbbbb 100644",
        )
        .replace(
            "@@ -7,7 +7,10 @@ pub struct Claims {",
            "@@ -70,7 +90,10 @@ another section",
        );
    assert_eq!(
        hash_of(JWT_V1, "src/auth/jwt.rs"),
        hash_of(&moved, "src/auth/jwt.rs")
    );
}

#[test]
fn a_changed_line_changes_the_hash() {
    for changed in [
        // An added, a removed and a context line.
        JWT_V1.replace(
            "+    if claims.exp <= now()",
            "+    if claims.exp + 1 <= now()",
        ),
        JWT_V1.replace("-    if claims.exp < now()", "-    if now() > claims.exp"),
        JWT_V1.replace("     let claims = decode", "     let c = decode"),
    ] {
        assert_ne!(
            hash_of(JWT_V1, "src/auth/jwt.rs"),
            hash_of(&changed, "src/auth/jwt.rs")
        );
    }
}

#[test]
fn hunk_lines_that_look_like_file_headers_are_hunk_lines() {
    // A removed `-- a` and an added `++ b` read as `--- a` and `+++ b` in a hunk.
    let patch = "\
diff --git a/q.sql b/q.sql
index 1..2 100644
--- a/q.sql
+++ b/q.sql
@@ -1 +1 @@
--- a
+++ b
";
    let other = patch.replace("+++ b\n", "+++ c\n");
    let files = file_hashes(patch.as_bytes());
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "q.sql");
    assert_ne!(hash_of(patch, "q.sql"), hash_of(&other, "q.sql"));
}

#[test]
fn the_end_of_line_of_a_changed_line_is_part_of_the_diff() {
    // The same text with CRLF, and the same lines with the marker of a missing final newline on
    // the other side: both are other diffs.
    let lf = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
    let crlf = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\r\n+b\r\n";
    assert_ne!(hash_of(lf, "x"), hash_of(crlf, "x"));
    let old_has_no_newline =
        "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+a\n";
    let new_has_no_newline =
        "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+a\n\\ No newline at end of file\n";
    assert_ne!(
        hash_of(old_has_no_newline, "x"),
        hash_of(new_has_no_newline, "x")
    );
}

#[test]
fn an_empty_line_in_a_hunk_is_a_context_line_and_a_final_newline_changes_nothing() {
    let git = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n \n-a\n+b\n";
    // Some tools strip the space of an empty context line, and GitHub ends a patch without a
    // newline.
    let stripped = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n\n-a\n+b";
    assert_eq!(hash_of(git, "x"), hash_of(stripped, "x"));
}

#[test]
fn a_file_without_hunks_is_compared_by_what_its_header_says() {
    let binary = |index: &str| {
        format!(
            "diff --git a/logo.png b/logo.png\nindex {index} 100644\nBinary files a/logo.png and b/logo.png differ\n"
        )
    };
    // Only the blobs tell two binary diffs apart.
    assert_ne!(
        hash_of(&binary("1..2"), "logo.png"),
        hash_of(&binary("1..3"), "logo.png")
    );
    let mode = |to: &str| format!("diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode {to}\n");
    assert_ne!(
        hash_of(&mode("100755"), "run.sh"),
        hash_of(&mode("120000"), "run.sh")
    );
    let renamed = |from: &str, score: u8| {
        format!(
            "diff --git a/{from} b/new.rs\nsimilarity index {score}%\nrename from {from}\nrename to new.rs\n"
        )
    };
    assert_eq!(
        hash_of(&renamed("old.rs", 100), "new.rs"),
        hash_of(&renamed("old.rs", 99), "new.rs")
    );
    assert_ne!(
        hash_of(&renamed("old.rs", 100), "new.rs"),
        hash_of(&renamed("older.rs", 100), "new.rs")
    );
}

#[test]
fn the_path_is_the_path_in_the_head() {
    for (patch, path) in [
        // Added, deleted, renamed, copied with changes.
        (
            "diff --git a/n b/n\nnew file mode 100644\nindex 0..1\n--- /dev/null\n+++ b/n\n@@ -0,0 +1 @@\n+a\n",
            "n",
        ),
        (
            "diff --git a/d b/d\ndeleted file mode 100644\nindex 1..0\n--- a/d\n+++ /dev/null\n@@ -1 +0,0 @@\n-a\n",
            "d",
        ),
        (
            "diff --git a/o b/n\nsimilarity index 90%\nrename from o\nrename to n\nindex 1..2\n--- a/o\n+++ b/n\n@@ -1 +1 @@\n-a\n+b\n",
            "n",
        ),
        (
            "diff --git a/o b/n\nsimilarity index 90%\ncopy from o\ncopy to n\n",
            "n",
        ),
        // Spaces: git ends the `---` and `+++` lines with a tab; no hunks: both halves are the same.
        (
            "diff --git a/my file b/my file\n--- a/my file\t\n+++ b/my file\t\n@@ -1 +1 @@\n-a\n+b\n",
            "my file",
        ),
        (
            "diff --git a/my file b/my file\nold mode 100644\nnew mode 100755\n",
            "my file",
        ),
        // Quoted by git.
        (
            "diff --git \"a/t\\tb\" \"b/t\\tb\"\n--- \"a/t\\tb\"\n+++ \"b/t\\tb\"\n@@ -1 +1 @@\n-a\n+b\n",
            "t\tb",
        ),
        (
            "diff --git \"a/\\303\\251t\\303\\251\" \"b/\\303\\251t\\303\\251\"\nold mode 100644\nnew mode 100755\n",
            "\u{e9}t\u{e9}",
        ),
    ] {
        let files = file_hashes(patch.as_bytes());
        assert_eq!(files.len(), 1, "{patch}");
        assert_eq!(files[0].path, path, "{patch}");
    }
}

#[test]
fn bytes_that_are_not_text_are_hashed_as_they_are() {
    let patch = |byte: u8| {
        let mut patch = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+".to_vec();
        patch.push(byte);
        patch.push(b'\n');
        patch
    };
    let hash = |byte| file_hashes(&patch(byte))[0].hash.clone();
    // Two invalid UTF-8 bytes: a lossy conversion would make them the same.
    assert_ne!(hash(0xe9), hash(0xe8));
}

#[test]
fn an_empty_patch_has_no_file() {
    assert_eq!(file_hashes(b""), []);
    assert_eq!(since_last_review(b"", b""), Vec::<String>::new());
}

#[test]
fn since_last_review_lists_changed_new_and_dropped_files_sorted() {
    let entry = |path: &str, line: &str| {
        format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-a\n+{line}\n"
        )
    };
    let reviewed = [
        entry("same", "b"),
        entry("z-dropped", "b"),
        entry("changed", "b"),
    ]
    .concat();
    let head = [
        entry("same", "b"),
        entry("changed", "c"),
        entry("a-new", "b"),
    ]
    .concat();
    assert_eq!(
        since_last_review(reviewed.as_bytes(), head.as_bytes()),
        ["a-new", "changed", "z-dropped"]
    );
    assert_eq!(
        since_last_review(head.as_bytes(), head.as_bytes()),
        Vec::<String>::new()
    );
}

#[test]
fn a_reduced_patch_hashes_as_the_same_diff_from_git() {
    let body = "@@ -1 +1 @@\n-a\n+b";
    let git = "diff --git a/x.rs b/x.rs\nindex 1..2 100644\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-a\n+b\n";
    let patch = reduced_patch(&[file("x.rs", FileChange::Modified, Some(body))]);
    assert_eq!(file_hashes(&patch), file_hashes(git.as_bytes()));
}

#[test]
fn a_reduced_patch_keeps_each_file_with_its_header() {
    let mut renamed = file("new.rs", FileChange::Renamed, Some("@@ -1 +1 @@\n-a\n+b\n"));
    renamed.old_path = Some("old.rs".to_owned());
    let patch = reduced_patch(&[
        file("add.rs", FileChange::Added, Some("@@ -0,0 +1 @@\n+a")),
        file("del.rs", FileChange::Deleted, Some("@@ -1 +0,0 @@\n-a")),
        renamed,
    ]);
    let text = String::from_utf8(patch.clone()).unwrap();
    assert!(text.contains("--- /dev/null\n+++ b/add.rs\n"), "{text}");
    assert!(text.contains("--- a/del.rs\n+++ /dev/null\n"), "{text}");
    assert!(
        text.contains("diff --git a/old.rs b/new.rs\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n"),
        "{text}"
    );
    assert!(text.ends_with('\n'));
    let paths: Vec<String> = file_hashes(&patch)
        .into_iter()
        .map(|file| file.path)
        .collect();
    assert_eq!(paths, ["add.rs", "del.rs", "new.rs"]);
}

#[test]
fn a_file_without_a_patch_from_the_forge_says_so_and_is_still_listed() {
    let mut large = file("big.json", FileChange::Modified, None);
    large.too_large = true;
    let patch = reduced_patch(&[large, file("logo.png", FileChange::Modified, None)]);
    let text = String::from_utf8(patch.clone()).unwrap();
    assert!(
        text.contains("# big.json: the forge gives no patch: the file is too large\n"),
        "{text}"
    );
    assert!(
        text.contains("# logo.png: the forge gives no patch\n"),
        "{text}"
    );
    assert_eq!(file_hashes(&patch).len(), 2);
}

/// The fixture of the force push, as `git diff -M <merge base> <head>` writes it.
#[test]
fn the_forge_patches_of_the_fixture_are_read_on_both_forges() {
    let github = crate::github::parse_files(include_bytes!(
        "../../../../fixtures/forge/github/pull-412-files.v2.json"
    ))
    .unwrap();
    let gitlab = crate::gitlab::parse_diffs(include_bytes!(
        "../../../../fixtures/forge/gitlab/merge-request-88-diffs.json"
    ))
    .unwrap();
    let github = file_hashes(&reduced_patch(&github));
    let paths: Vec<&str> = github.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["Cargo.lock", "src/api/login.rs", "src/auth/jwt.rs"]);
    assert_eq!(github, file_hashes(&reduced_patch(&gitlab)));
}

#[test]
fn a_file_without_a_patch_has_no_hash_and_always_counts_as_changed() {
    let mut large = file("big.json", FileChange::Modified, None);
    large.too_large = true;
    let binary = file("logo.png", FileChange::Modified, None);
    let same = reduced_patch(&[large, binary]);
    let files = file_hashes(&same);
    assert_eq!(files[0].hash, None);
    assert_eq!(files[1].hash, None);
    // Nothing says that the two versions are the same.
    assert_eq!(since_last_review(&same, &same), ["big.json", "logo.png"]);
    // On one side only: a local binary diff against the forge's silence.
    let local = b"diff --git a/logo.png b/logo.png\nindex 1..2 100644\nBinary files a/logo.png and b/logo.png differ\n";
    let reduced = reduced_patch(&[file("logo.png", FileChange::Modified, None)]);
    assert_eq!(since_last_review(local, &reduced), ["logo.png"]);
    assert_eq!(since_last_review(&reduced, local), ["logo.png"]);
}

#[test]
fn added_deleted_and_renamed_files_hash_the_same_from_git_and_from_the_forge() {
    let added = "diff --git a/n.rs b/n.rs\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/n.rs\n@@ -0,0 +1 @@\n+a\n";
    let deleted = "diff --git a/d.rs b/d.rs\ndeleted file mode 100644\nindex 1111111..0000000\n--- a/d.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-a\n";
    let edited = "diff --git a/o.rs b/n2.rs\nsimilarity index 80%\nrename from o.rs\nrename to n2.rs\nindex 1..2 100644\n--- a/o.rs\n+++ b/n2.rs\n@@ -1 +1 @@\n-a\n+b\n";
    let pure =
        "diff --git a/p.rs b/q.rs\nsimilarity index 100%\nrename from p.rs\nrename to q.rs\n";
    let git = [added, deleted, edited, pure].concat();

    let renamed = |path: &str, old: &str, body: Option<&str>| FileDiff {
        old_path: Some(old.to_owned()),
        ..file(path, FileChange::Renamed, body)
    };
    let forge = reduced_patch(&[
        file("n.rs", FileChange::Added, Some("@@ -0,0 +1 @@\n+a")),
        file("d.rs", FileChange::Deleted, Some("@@ -1 +0,0 @@\n-a")),
        renamed("n2.rs", "o.rs", Some("@@ -1 +1 @@\n-a\n+b")),
        // A rename alone has no patch, and is not cut: nothing is unknown about it.
        renamed("q.rs", "p.rs", None),
    ]);
    assert_eq!(file_hashes(&forge), file_hashes(git.as_bytes()));
    assert_eq!(
        since_last_review(git.as_bytes(), &forge),
        Vec::<String>::new()
    );
}

#[test]
fn a_rename_from_another_file_or_a_cut_rename_counts_as_changed() {
    let renamed = |old: &str, too_large: bool| FileDiff {
        old_path: Some(old.to_owned()),
        too_large,
        ..file("q.rs", FileChange::Renamed, None)
    };
    let from_p = reduced_patch(&[renamed("p.rs", false)]);
    let from_r = reduced_patch(&[renamed("r.rs", false)]);
    assert_eq!(since_last_review(&from_p, &from_r), ["q.rs"]);
    let cut = reduced_patch(&[renamed("p.rs", true)]);
    assert_eq!(since_last_review(&cut, &cut), ["q.rs"]);
}

#[test]
fn a_new_file_with_hunks_ignores_its_mode_but_a_file_without_hunks_does_not() {
    let added = |mode: &str| {
        format!(
            "diff --git a/n b/n\nnew file mode {mode}\n--- /dev/null\n+++ b/n\n@@ -0,0 +1 @@\n+a\n"
        )
    };
    assert_eq!(
        hash_of(&added("100644"), "n"),
        hash_of(&added("100755"), "n")
    );
    let empty = |mode: &str| format!("diff --git a/n b/n\nnew file mode {mode}\nindex 0..e69de\n");
    assert_ne!(
        hash_of(&empty("100644"), "n"),
        hash_of(&empty("100755"), "n")
    );
}

#[test]
fn a_special_byte_in_a_forge_path_cannot_forge_a_section() {
    let body = Some("@@ -1 +1 @@\n-a\n+b");
    let evil = "x\ndiff --git a/fake b/fake";
    let patch = reduced_patch(&[file(evil, FileChange::Modified, body)]);
    let files = file_hashes(&patch);
    assert_eq!(files.len(), 1, "{}", String::from_utf8_lossy(&patch));
    assert_eq!(files[0].path, evil);
    let spaced = reduced_patch(&[file("a b\tc\"d", FileChange::Modified, body)]);
    assert_eq!(file_hashes(&spaced)[0].path, "a b\tc\"d");
    let none = reduced_patch(&[file(evil, FileChange::Modified, None)]);
    assert_eq!(file_hashes(&none).len(), 1);
    assert_eq!(file_hashes(&none)[0].path, evil);
}

#[test]
fn a_reduced_patch_over_the_limit_only_lists_its_files() {
    let body = Some("@@ -1 +1 @@\n-a\n+b");
    let files = [
        file("a.rs", FileChange::Modified, body),
        file("b.rs", FileChange::Modified, body),
    ];
    let full = reduced_patch(&files);
    assert_eq!(reduced_patch_within(&files, full.len()), full);
    let cut = reduced_patch_within(&files, full.len() - 1);
    let text = String::from_utf8(cut.clone()).unwrap();
    assert!(!text.contains("@@"), "{text}");
    assert!(
        text.contains("# a.rs: the forge gives no patch: the file is too large\n"),
        "{text}"
    );
    let hashes = file_hashes(&cut);
    assert_eq!(hashes.len(), 2);
    assert!(hashes.iter().all(|file| file.hash.is_none()));
}
