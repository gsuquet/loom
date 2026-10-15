# Loom Documentation & Code Style Guide

> The rules for code, documentation and commit messages in Project Loom.

---

## 1. Rust Code Style Standards

- **Format:** Format the code with `cargo fmt`. Do not commit code that is not formatted.
- **Linter:** `clippy` runs with the `pedantic` and `nursery` groups. It is part of `mise run check`.
- **Zero Panics:** Library crates (`loom-core`) do not use `unwrap()` or `expect()`. Use `Result<T, DomainError>`.
- **Visibility:** Make items private by default. Give each public API complete Rustdoc comments (`///`).
- **Error Types:** Use `thiserror` in libraries and `miette` in the CLI binary.

---

## 2. Documentation Style

### 2.1 General rules

- **Voice:** Write directly to the reader. Use the active voice.
- **Structure:** Use a clear hierarchy of headings. Write tasks as numbered steps and give concrete code examples.
- **Headings:** Use sentence case or Title Case. Use the same case in all the headings of a page.
- **Code Blocks:** Give the language of each code block (for example ````rust````, ````bash````, ````toml````).

### 2.2 Simplified Technical English (ASD-STE100)

The user documentation follows the rules of [ASD-STE100 Simplified Technical English](https://www.asd-ste100.org/), a specification of the AeroSpace and Defence Industries Association of Europe (ASD).

**Where the rules apply:**

- `docs/reference/`. The CLI reference (`docs/reference/cli/loom.md`, `loomd.md`) and the JSON Schemas (`docs/reference/schemas/`) are generated. To change their text, change the doc comments in the Rust source, then run `mise run docs`.
- `docs/how-to-guides/`, `docs/explanation/` and `docs/tutorials/`.
- `README.md`.

**Where the rules do not apply:**

- `docs/adr/`: the decision records stay as they were written.
- `docs/design/`: the target-behavior specifications do not follow the rules yet.
- `CHANGELOG.md`, `AGENTS.md` and `CLAUDE.md`.

#### Words

- Use simple words that have one meaning. For one meaning, use one word, and use it each time. For example, if a page says "context store", do not also say "context repository".
- Use a word as one part of speech only. For example, if you use "update" as a noun, do not also use it as a verb in the same page.
- Keep technical names as they are: commands, flags, keys, file names, fields, error codes, product names. Write them in code format when they are code.
- Do not use an -ing form as a noun or as an adjective. Write "before you install", not "before installing". Write "the daemon that runs", not "the running daemon". An -ing form in a technical name stays as it is.
- Do not use phrasal verbs such as "set up" or "carry out". Use one verb: "configure", "do". The name of a command stays as it is.
- Keep the articles ("a", "an", "the") in sentences. Do not use contractions ("don't", "it's").

#### Sentences

- Write one topic in each sentence.
- A sentence in a procedure has a maximum of 20 words. A descriptive sentence has a maximum of 25 words. A code span, a link or a technical name counts as one word.
- Use the present tense. Use the past or the future tense only when the time is necessary.
- Use the active voice. In a procedure, always use the active voice. In descriptive text, use the passive voice only when the agent is not known or not important.
- Put a condition before the result: "If the file is not there, `loom` stops".

#### Procedures

- Write each instruction in the imperative.
- Write one instruction in each sentence. Two actions that you do at the same time can be in one sentence.
- Put the condition before the instruction: "If the check fails, do the backup again".
- Use a numbered list for a sequence of steps. Use a bulleted list for items that have no order.
- Put a warning or a caution before the step that it applies to. First give the instruction, then give the risk.

#### Paragraphs

- Write one topic in each paragraph.
- A paragraph has a maximum of 6 sentences.
- Use a vertical list for a sequence, and for three or more items that the reader must compare.

#### Tables and lists

The cells of a table and the items of a reference list can be short noun phrases. These fragments do not need articles. A full sentence in a cell follows all the rules above.

#### Vocabulary check

The STE dictionary gives the approved words with their approved meaning and part of speech. ASD supplies the specification on request. This project does not check the vocabulary word by word against the dictionary. Writers apply the rules above and use simple, common words with one meaning each. Reviewers check the rules in each pull request: no tool checks them.

---

## 3. Git & Review Standards

- **Commit Messages:** Use [Conventional Commits](https://www.conventionalcommits.org):
  - Format: `<type>(<scope>): <short description>`
  - Types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`.
- **Code Reviews:** Use [Conventional Comments](https://conventionalcomments.org):
  - Labels: `issue`, `suggestion`, `nitpick`, `todo`, `thought`, `praise`.
  - Decorations: `(blocking)`, `(non-blocking)`, `(if-minor)`.
