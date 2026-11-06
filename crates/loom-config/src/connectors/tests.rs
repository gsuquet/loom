use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use loom_api::{ConnectorKind, ContextName};

use super::{
    Connector, ConnectorName, ConnectorSettings, DatadogSettings, FreshserviceSettings,
    JiraSettings, MonitorState, RawConnector,
};
use crate::config::{Config, ConfigError, parse_config};
use crate::forges::TokenSource;

const PATH: &str = "/home/me/.loom/config.toml";

fn parse(text: &str) -> Config {
    parse_config(text, Path::new(PATH)).unwrap()
}

fn refusal(text: &str) -> ConfigError {
    parse_config(text, Path::new(PATH)).unwrap_err()
}

/// The refusal has this line and holds `needle`.
#[track_caller]
fn assert_refused(text: &str, line: usize, needle: &str) {
    let error = refusal(text);
    assert_eq!(
        error.location.map(|at| at.line),
        Some(line),
        "{text}\n{error}"
    );
    assert!(error.message.contains(needle), "{text}\n{error}");
}

fn connector<'a>(config: &'a Config, context: &str, name: &str) -> &'a Connector {
    let context: ContextName = context.parse().unwrap();
    let name: ConnectorName = name.to_owned().try_into().unwrap();
    config.contexts[&context].connectors[&name].get_ref()
}

fn datadog(connector: &Connector) -> &DatadogSettings {
    match &connector.settings {
        ConnectorSettings::Datadog(settings) => settings,
        other => panic!("{other:?}"),
    }
}

fn jira(connector: &Connector) -> &JiraSettings {
    match &connector.settings {
        ConnectorSettings::Jira(settings) => settings,
        other => panic!("{other:?}"),
    }
}

fn freshservice(connector: &Connector) -> &FreshserviceSettings {
    match &connector.settings {
        ConnectorSettings::Freshservice(settings) => settings,
        other => panic!("{other:?}"),
    }
}

/// The keys a table of the kind needs, on the lines after its header.
const DATADOG: &str = "kind = \"datadog\"\nsite = \"datadoghq.eu\"\ntoken_env = \"DD_API\"\napp_key_env = \"DD_APP\"\nquery = \"env:prod\"\n";
const JIRA: &str = "kind = \"jira\"\nsite = \"company-a.atlassian.net\"\ntoken_env = \"JIRA_TOKEN\"\nemail = \"me@company-a.example\"\n";
const FRESHSERVICE: &str = "kind = \"freshservice\"\nsite = \"company-a.freshservice.com\"\ntoken_env = \"FS_TOKEN\"\nagent_id = 12000000001\n";

fn table(name: &str, body: &str) -> String {
    format!("[contexts.a.connectors.{name}]\n{body}")
}

const FULL: &str = r#"
[contexts.company-a.connectors.datadog]
kind = "datadog"
site = "datadoghq.eu"
token_cmd = "op read op://company-a/datadog/api-key"
app_key_cmd = "op read op://company-a/datadog/app-key"
query = 'tag:"team:payments" env:prod'
states = ["alert", "warn", "no data"]
priorities = [1, 2, 3]
urgent_priorities = [1]
flap_window = "1h"
poll_interval = "2m"

[contexts.company-a.connectors.jira]
kind = "jira"
site = "company-a.atlassian.net"
email = "me@company-a.example"
token_cmd = "op read op://company-a/jira/token"
list_jql = "assignee = currentUser() ORDER BY key"
pool_jql = "project = OPS AND assignee is EMPTY ORDER BY created DESC, key"
poll_interval = "6m"
pool_interval = "20m"
pool_limit = 50

[contexts.company-a.connectors.freshservice]
kind = "freshservice"
site = "company-a.freshservice.com"
token_env = "LOOM_FRESHSERVICE_TOKEN"
agent_id = 12000000001
group_ids = [12000000002]
workspace_id = 2
open_statuses = [2, 3, 7]
extra_query = "tag:'payments'"
pool_interval = "30m"
pool_limit = 10
"#;

#[test]
fn every_key_of_each_kind_is_read() {
    let config = parse(FULL);

    let instance = connector(&config, "company-a", "datadog");
    assert_eq!(instance.kind(), ConnectorKind::Datadog);
    assert_eq!(instance.site.as_str(), "datadoghq.eu");
    assert_eq!(
        instance.token,
        TokenSource::Command("op read op://company-a/datadog/api-key".to_owned())
    );
    assert_eq!(instance.poll_interval.get(), Duration::from_mins(2));
    let settings = datadog(instance);
    assert_eq!(
        settings.app_key,
        TokenSource::Command("op read op://company-a/datadog/app-key".to_owned())
    );
    assert_eq!(settings.query, "tag:\"team:payments\" env:prod");
    assert_eq!(
        settings.states,
        BTreeSet::from([
            MonitorState::Alert,
            MonitorState::Warn,
            MonitorState::NoData
        ])
    );
    assert_eq!(settings.priorities, Some(BTreeSet::from([1, 2, 3])));
    assert_eq!(settings.urgent_priorities, BTreeSet::from([1]));
    assert_eq!(settings.flap_window, Duration::from_hours(1));

    let instance = connector(&config, "company-a", "jira");
    assert_eq!(instance.kind(), ConnectorKind::Jira);
    assert_eq!(instance.poll_interval.get(), Duration::from_mins(6));
    let settings = jira(instance);
    assert_eq!(settings.email, "me@company-a.example");
    assert_eq!(settings.list_jql, "assignee = currentUser() ORDER BY key");
    assert_eq!(
        settings.pool_jql.as_deref(),
        Some("project = OPS AND assignee is EMPTY ORDER BY created DESC, key")
    );
    assert_eq!(settings.pool.interval.get(), Duration::from_mins(20));
    assert_eq!(settings.pool.limit, 50);

    let instance = connector(&config, "company-a", "freshservice");
    assert_eq!(instance.kind(), ConnectorKind::Freshservice);
    assert_eq!(
        instance.token,
        TokenSource::Env("LOOM_FRESHSERVICE_TOKEN".to_owned())
    );
    let settings = freshservice(instance);
    assert_eq!(settings.agent_id, 12_000_000_001);
    assert_eq!(settings.group_ids, Some(vec![12_000_000_002]));
    assert_eq!(settings.workspace_id, Some(2));
    assert_eq!(settings.open_statuses, BTreeSet::from([2, 3, 7]));
    assert_eq!(settings.extra_query.as_deref(), Some("tag:'payments'"));
    assert_eq!(settings.pool.interval.get(), Duration::from_mins(30));
    assert_eq!(settings.pool.limit, 10);
}

#[test]
fn defaults_of_each_kind() {
    let config = parse(&format!(
        "{}{}{}",
        table("datadog", DATADOG),
        table("jira", JIRA),
        table("freshservice", FRESHSERVICE)
    ));

    let instance = connector(&config, "a", "datadog");
    assert_eq!(instance.poll_interval.get(), Duration::from_mins(1));
    let settings = datadog(instance);
    assert_eq!(
        settings.states,
        BTreeSet::from([MonitorState::Alert, MonitorState::Warn])
    );
    assert_eq!(settings.priorities, None);
    assert_eq!(settings.urgent_priorities, BTreeSet::from([1, 2]));
    assert_eq!(settings.flap_window, Duration::from_mins(30));

    let instance = connector(&config, "a", "jira");
    assert_eq!(instance.poll_interval.get(), Duration::from_mins(5));
    let settings = jira(instance);
    assert_eq!(
        settings.list_jql,
        "assignee = currentUser() AND statusCategory != Done ORDER BY key"
    );
    assert_eq!(settings.pool_jql, None);
    assert_eq!(settings.pool.interval.get(), Duration::from_mins(15));
    assert_eq!(settings.pool.limit, 200);

    let instance = connector(&config, "a", "freshservice");
    assert_eq!(instance.poll_interval.get(), Duration::from_mins(5));
    let settings = freshservice(instance);
    assert_eq!(settings.group_ids, None);
    assert_eq!(settings.workspace_id, None);
    assert_eq!(settings.open_statuses, BTreeSet::from([2, 3]));
    assert_eq!(settings.extra_query, None);
    assert_eq!(settings.pool.interval.get(), Duration::from_mins(15));
    assert_eq!(settings.pool.limit, 200);
}

#[test]
fn the_api_address_follows_the_kind_and_the_site() {
    let config = parse(&format!(
        "{}{}{}",
        table("datadog", DATADOG),
        table("jira", JIRA),
        table("freshservice", FRESHSERVICE)
    ));
    assert_eq!(
        connector(&config, "a", "datadog").api_base(),
        "https://api.datadoghq.eu"
    );
    assert_eq!(
        connector(&config, "a", "jira").api_base(),
        "https://company-a.atlassian.net/rest/api/3"
    );
    assert_eq!(
        connector(&config, "a", "freshservice").api_base(),
        "https://company-a.freshservice.com/api/v2"
    );
}

/// The first TOML block of `page` after `heading`.
fn example<'a>(page: &'a str, heading: &str) -> &'a str {
    let (_, rest) = page.split_once(heading).unwrap();
    let (_, rest) = rest.split_once("```toml\n").unwrap();
    rest.split_once("```").unwrap().0
}

#[test]
fn the_examples_of_the_docs_parse() {
    for (page, heading) in [
        (
            include_str!("../../../../docs/reference/configuration.md"),
            "### Connectors",
        ),
        (
            include_str!("../../../../docs/design/connector-ports.md"),
            "## Connector instances",
        ),
    ] {
        let config = parse(example(page, heading));
        for name in ["datadog", "jira", "freshservice"] {
            connector(&config, "company-a", name);
        }
    }
}

#[test]
fn a_config_without_a_connector_has_none() {
    let config = parse("[contexts.a]\n");
    assert!(
        config.contexts[&"a".parse::<ContextName>().unwrap()]
            .connectors
            .is_empty()
    );
}

#[test]
fn an_unknown_key_is_refused_at_its_line() {
    assert_refused(
        &table("jira", &format!("{JIRA}colour = \"red\"\n")),
        6,
        "unknown field `colour`",
    );
}

#[test]
fn a_key_of_another_kind_is_refused_at_its_line() {
    for (body, key, kind) in [
        (DATADOG, "email = \"me@example.com\"", "datadog"),
        (DATADOG, "list_jql = \"x\"", "datadog"),
        (DATADOG, "pool_jql = \"x\"", "datadog"),
        (DATADOG, "pool_interval = \"15m\"", "datadog"),
        (DATADOG, "pool_limit = 1", "datadog"),
        (DATADOG, "agent_id = 1", "datadog"),
        (DATADOG, "group_ids = [1]", "datadog"),
        (DATADOG, "workspace_id = 1", "datadog"),
        (DATADOG, "open_statuses = [2]", "datadog"),
        (DATADOG, "extra_query = \"x\"", "datadog"),
        (JIRA, "app_key_cmd = \"x\"", "jira"),
        (JIRA, "app_key_env = \"X\"", "jira"),
        (JIRA, "query = \"x\"", "jira"),
        (JIRA, "states = [\"alert\"]", "jira"),
        (JIRA, "priorities = [1]", "jira"),
        (JIRA, "urgent_priorities = [1]", "jira"),
        (JIRA, "flap_window = \"30m\"", "jira"),
        (JIRA, "agent_id = 1", "jira"),
        (JIRA, "group_ids = [1]", "jira"),
        (JIRA, "workspace_id = 1", "jira"),
        (JIRA, "open_statuses = [2]", "jira"),
        (JIRA, "extra_query = \"x\"", "jira"),
        (FRESHSERVICE, "app_key_env = \"X\"", "freshservice"),
        (FRESHSERVICE, "query = \"x\"", "freshservice"),
        (FRESHSERVICE, "flap_window = \"30m\"", "freshservice"),
        (FRESHSERVICE, "email = \"me@example.com\"", "freshservice"),
        (FRESHSERVICE, "list_jql = \"x\"", "freshservice"),
        (FRESHSERVICE, "pool_jql = \"x\"", "freshservice"),
    ] {
        let name = key.split(' ').next().unwrap();
        let text = table("x", &format!("{body}{key}\n"));
        let line = text.lines().count();
        assert_refused(
            &text,
            line,
            &format!("`{name}` is not a key of a `{kind}` connector"),
        );
    }
}

#[test]
fn the_pool_keys_are_for_jira_and_freshservice() {
    for body in [JIRA, FRESHSERVICE] {
        parse(&table(
            "x",
            &format!("{body}pool_interval = \"1m\"\npool_limit = 1\n"),
        ));
    }
}

#[test]
fn the_kind_is_required_and_closed() {
    assert_refused(
        "[contexts.a.connectors.x]\nsite = \"datadoghq.eu\"\n",
        1,
        "missing field `kind`",
    );
    for kind in ["gitlab", "github", "splunk"] {
        assert_refused(
            &format!("[contexts.a.connectors.x]\nkind = \"{kind}\"\nsite = \"x.example\"\n"),
            2,
            "unknown variant",
        );
    }
}

#[test]
fn the_name_is_lowercase_letters_digits_and_underscore() {
    for name in ["jira", "jira_2", "2", "_", &"a".repeat(64)] {
        parse(&table(name, JIRA));
    }
    for name in [
        "jira-2",
        "Jira",
        "\"jira.eu\"",
        "\"\"",
        "\"a b\"",
        "\"jira/x\"",
        "\"é\"",
        &"a".repeat(65),
    ] {
        assert_refused(&table(name, JIRA), 1, "invalid connector name");
    }
}

#[test]
fn a_name_equal_to_a_forge_host_of_its_context_is_refused() {
    // By the context's own table.
    let text = format!(
        "[forges.gitlab]\nkind = \"gitlab\"\n\n[contexts.a.forges.gitlab]\ntoken_env = \"T\"\n\n{}",
        table("gitlab", JIRA)
    );
    assert_refused(&text, 7, "`gitlab` is a forge host of context `a`");
    // By the shared table.
    let text = format!(
        "[forges.gitlab]\nkind = \"gitlab\"\ntoken_cmd = \"t\"\n\n{}",
        table("gitlab", JIRA)
    );
    assert_refused(&text, 5, "`gitlab` is a forge host of context `a`");
}

#[test]
fn a_name_equal_to_a_host_with_no_credential_there_is_accepted() {
    // A table with only `kind` gives no status row.
    parse(&format!(
        "[forges.gitlab]\nkind = \"gitlab\"\n\n{}",
        table("gitlab", JIRA)
    ));
    // The credential is of another context.
    parse(&format!(
        "[forges.gitlab]\nkind = \"gitlab\"\n\n[contexts.b.forges.gitlab]\ntoken_env = \"T\"\n\n{}",
        table("gitlab", JIRA)
    ));
}

/// `body` with this `site` as its first key, on line 2 of a table.
fn with_site(body: &str, site: &str) -> String {
    let line = format!("site = \"{site}\"\n");
    format!("{line}{}", without(body, "site"))
}

#[test]
fn every_published_datadog_site_is_accepted() {
    for site in [
        "datadoghq.com",
        "us3.datadoghq.com",
        "us5.datadoghq.com",
        "ap1.datadoghq.com",
        "ap2.datadoghq.com",
        "uk1.datadoghq.com",
        "datadoghq.eu",
        "ddog-gov.com",
        "us2.ddog-gov.com",
    ] {
        let config = parse(&table("x", &with_site(DATADOG, site)));
        assert_eq!(
            connector(&config, "a", "x").api_base(),
            format!("https://api.{site}")
        );
    }
}

#[test]
fn a_site_outside_the_rule_of_its_kind_is_refused() {
    for (body, site) in [
        (DATADOG, "us9.datadoghq.com"),
        (DATADOG, "api.datadoghq.com"),
        (DATADOG, "datadoghq.eu.company-a.example"),
        (DATADOG, "company-a.atlassian.net"),
        (JIRA, "atlassian.net"),
        (JIRA, "company-a.atlassian.net.company-b.example"),
        (JIRA, "company-aatlassian.net"),
        (JIRA, "company-a.freshservice.com"),
        (JIRA, "jira.company-a.example"),
        (FRESHSERVICE, "freshservice.com"),
        (FRESHSERVICE, "company-a.freshservice.com.company-b.example"),
        (FRESHSERVICE, "company-afreshservice.com"),
        (FRESHSERVICE, "company-a.atlassian.net"),
    ] {
        assert_refused(&table("x", &with_site(body, site)), 2, "site");
    }
}

#[test]
fn a_site_that_is_not_a_bare_hostname_is_refused() {
    for (body, suffix) in [
        (JIRA, ".atlassian.net"),
        (FRESHSERVICE, ".freshservice.com"),
    ] {
        for prefix in [
            "https://a",
            "me@a",
            "me:secret@a",
            "a:443/x",
            "a/x",
            "a?x",
            "a#x",
            "a b",
            "A",
            "a..b",
            "-a",
            "a-",
            "a.-b",
            "a%2Eb",
            "é",
            ".",
            "",
        ] {
            let site = format!("{prefix}{suffix}");
            assert_refused(
                &table("x", &with_site(body, &site)),
                2,
                "expected a lowercase host name",
            );
        }
        let long = format!("{}{suffix}", "a".repeat(64));
        assert_refused(
            &table("x", &with_site(body, &long)),
            2,
            "expected a lowercase host name",
        );
    }
    for site in [
        "",
        "https://datadoghq.eu",
        "datadoghq.eu:443",
        "datadoghq.eu.",
    ] {
        assert_refused(
            &table("x", &with_site(DATADOG, site)),
            2,
            "expected a lowercase host name",
        );
    }
}

#[test]
fn a_long_site_label_of_63_characters_is_accepted() {
    let site = format!("{}.atlassian.net", "a".repeat(63));
    parse(&table("x", &with_site(JIRA, &site)));
}

/// The keys of `body`, without the line of `key`.
fn without(body: &str, key: &str) -> String {
    body.lines()
        .filter(|line| !line.starts_with(key))
        .flat_map(|line| [line, "\n"])
        .collect()
}

#[test]
fn a_missing_required_key_is_refused_at_the_table() {
    for (body, key, needle) in [
        (DATADOG, "site", "missing field `site`"),
        (JIRA, "site", "missing field `site`"),
        (DATADOG, "token_env", "expected token_cmd or token_env"),
        (JIRA, "token_env", "expected token_cmd or token_env"),
        (FRESHSERVICE, "token_env", "expected token_cmd or token_env"),
        (
            DATADOG,
            "app_key_env",
            "expected app_key_cmd or app_key_env",
        ),
        (DATADOG, "query", "a `datadog` connector needs `query`"),
        (JIRA, "email", "a `jira` connector needs `email`"),
        (
            FRESHSERVICE,
            "agent_id",
            "a `freshservice` connector needs `agent_id`",
        ),
    ] {
        assert_refused(&table("x", &without(body, key)), 1, needle);
    }
}

#[test]
fn each_secret_has_one_source_with_the_rules_of_a_forge() {
    let token =
        |extra: &str, body: &str| table("x", &format!("{}{extra}\n", without(body, "token_env")));
    for (text, needle) in [
        (
            table("x", &format!("{JIRA}token_cmd = \"t\"\n")),
            "token_cmd and token_env are both set",
        ),
        (token("token_cmd = \" \"", JIRA), "token_cmd is empty"),
        (token("token_env = \"1T\"", JIRA), "invalid token_env `1T`"),
        (
            table("x", &format!("{DATADOG}app_key_cmd = \"t\"\n")),
            "app_key_cmd and app_key_env are both set",
        ),
        (
            table(
                "x",
                &format!("{}app_key_cmd = \" \"\n", without(DATADOG, "app_key_env")),
            ),
            "app_key_cmd is empty",
        ),
        (
            table(
                "x",
                &format!("{}app_key_env = \"A=B\"\n", without(DATADOG, "app_key_env")),
            ),
            "invalid app_key_env `A=B`",
        ),
    ] {
        assert_refused(&text, 1, needle);
    }
}

#[test]
fn intervals_have_a_minimum_of_one_minute() {
    for (body, key) in [
        (DATADOG, "poll_interval"),
        (JIRA, "poll_interval"),
        (FRESHSERVICE, "poll_interval"),
        (JIRA, "pool_interval"),
        (FRESHSERVICE, "pool_interval"),
        (DATADOG, "flap_window"),
    ] {
        parse(&table("x", &format!("{body}{key} = \"1m\"\n")));
        let text = table("x", &format!("{body}{key} = \"59s\"\n"));
        assert_refused(
            &text,
            text.lines().count(),
            &format!("{key} `59s` is shorter than the minimum 1m"),
        );
        let text = table("x", &format!("{body}{key} = \"soon\"\n"));
        assert_refused(
            &text,
            text.lines().count(),
            &format!("{key}: invalid duration `soon`"),
        );
    }
}

#[test]
fn invalid_values_are_refused_at_their_line() {
    for (body, line, needle) in [
        (DATADOG, "query = \" \"", "query is blank"),
        (DATADOG, "states = []", "states is empty"),
        (DATADOG, "states = [\"ok\"]", "unknown variant `ok`"),
        (DATADOG, "priorities = []", "priorities is empty"),
        (
            DATADOG,
            "priorities = [0]",
            "priorities: 0 is not from 1 to 5",
        ),
        (
            DATADOG,
            "priorities = [6]",
            "priorities: 6 is not from 1 to 5",
        ),
        (
            DATADOG,
            "urgent_priorities = [0]",
            "urgent_priorities: 0 is not from 1 to 5",
        ),
        (
            DATADOG,
            "urgent_priorities = [6]",
            "urgent_priorities: 6 is not from 1 to 5",
        ),
        (JIRA, "email = \"\"", "invalid email"),
        (JIRA, "email = \"me\"", "invalid email"),
        (JIRA, "email = \"@example.com\"", "invalid email"),
        (JIRA, "email = \"me@\"", "invalid email"),
        (JIRA, "email = \"a@b@example.com\"", "invalid email"),
        (JIRA, "email = \"m e@example.com\"", "invalid email"),
        (JIRA, "email = \"me:x@example.com\"", "invalid email"),
        (JIRA, "email = \"me\\n@example.com\"", "invalid email"),
        (JIRA, "list_jql = \" \"", "list_jql is blank"),
        (JIRA, "pool_jql = \"\"", "pool_jql is blank"),
        (JIRA, "pool_limit = 0", "pool_limit is 0"),
        (FRESHSERVICE, "pool_limit = 0", "pool_limit is 0"),
        (FRESHSERVICE, "agent_id = 0", "agent_id is 0"),
        (FRESHSERVICE, "group_ids = []", "group_ids is empty"),
        (
            FRESHSERVICE,
            "group_ids = [1, 0]",
            "group_ids: 0 is not an ID",
        ),
        (FRESHSERVICE, "open_statuses = []", "open_statuses is empty"),
        (
            FRESHSERVICE,
            "open_statuses = [0]",
            "open_statuses: 0 is not a status",
        ),
        (
            FRESHSERVICE,
            "open_statuses = [2, 4]",
            "open_statuses: 4 is terminal",
        ),
        (
            FRESHSERVICE,
            "open_statuses = [5]",
            "open_statuses: 5 is terminal",
        ),
        (FRESHSERVICE, "extra_query = \"\"", "extra_query is blank"),
        (FRESHSERVICE, "workspace_id = -1", "expected u64"),
    ] {
        let name = line.split(' ').next().unwrap();
        let text = table("x", &format!("{}{line}\n", without(body, name)));
        assert_refused(&text, text.lines().count(), needle);
    }
}

#[test]
fn the_edges_of_the_values_are_accepted() {
    for (body, line) in [
        (DATADOG, "priorities = [1, 5]"),
        (DATADOG, "urgent_priorities = [1, 5]"),
        (DATADOG, "urgent_priorities = []"),
        (FRESHSERVICE, "group_ids = [1]"),
        (FRESHSERVICE, "workspace_id = 0"),
        (FRESHSERVICE, "open_statuses = [1, 3, 6]"),
        (JIRA, "pool_limit = 1"),
    ] {
        parse(&table("x", &format!("{body}{line}\n")));
    }
    let config = parse(&table(
        "x",
        &format!("{}agent_id = 1\n", without(FRESHSERVICE, "agent_id")),
    ));
    assert_eq!(freshservice(connector(&config, "a", "x")).agent_id, 1);
}

#[test]
fn two_instances_of_one_context_on_one_site_are_refused() {
    for body in [JIRA, FRESHSERVICE] {
        let text = format!("{}\n{}", table("one", body), table("two", body));
        let line = table("one", body).lines().count() + 2;
        assert_refused(&text, line, "`one` of the same context has the same site");
    }
    // Datadog's site says nothing of the organization.
    parse(&format!(
        "{}\n{}",
        table("one", DATADOG),
        table("two", DATADOG)
    ));
    // Another site is another instance.
    parse(&format!(
        "{}\n{}",
        table("one", JIRA),
        table("two", &with_site(JIRA, "company-b.atlassian.net"))
    ));
}

fn in_context(context: &str, name: &str, body: &str) -> String {
    format!("[contexts.{context}.connectors.{name}]\n{body}\n")
}

#[test]
fn two_contexts_on_one_site_must_each_say_what_is_theirs() {
    let own_jql = |jql: &str| format!("{JIRA}list_jql = \"{jql}\"\n");
    let own_workspace = |id: u32| format!("{FRESHSERVICE}workspace_id = {id}\n");
    let own_query = |query: &str| format!("{FRESHSERVICE}extra_query = \"{query}\"\n");

    // Every context says it.
    parse(&format!(
        "{}{}",
        in_context("a", "x", &own_jql("project = A")),
        in_context("b", "x", &own_jql("project = B")),
    ));
    parse(&format!(
        "{}{}{}",
        in_context("a", "x", &own_workspace(1)),
        in_context("b", "x", &own_workspace(2)),
        in_context("c", "x", &own_query("tag:'c'")),
    ));
    // One workspace, split by the query.
    parse(&format!(
        "{}{}",
        in_context(
            "a",
            "x",
            &format!("{}extra_query = \"tag:'a'\"\n", own_workspace(1))
        ),
        in_context(
            "b",
            "x",
            &format!("{}extra_query = \"tag:'b'\"\n", own_workspace(1))
        ),
    ));

    let default_jql = "assignee = currentUser() AND statusCategory != Done ORDER BY key";
    // `refused` is the context of the refusal.
    for (first, second, refused, needle) in [
        // The default on one side only, either side.
        (JIRA.to_owned(), own_jql("project = B"), "a", "list_jql"),
        (own_jql("project = A"), JIRA.to_owned(), "b", "list_jql"),
        // The default's own text is the default.
        (
            own_jql(default_jql),
            own_jql("project = B"),
            "a",
            "list_jql",
        ),
        (
            FRESHSERVICE.to_owned(),
            own_workspace(2),
            "a",
            "workspace_id or extra_query",
        ),
        (
            own_query("tag:'a'"),
            FRESHSERVICE.to_owned(),
            "b",
            "workspace_id or extra_query",
        ),
        // The same filter says nothing.
        (
            own_jql("project = A"),
            own_jql("project = A"),
            "b",
            "the same filter",
        ),
        (own_workspace(1), own_workspace(1), "b", "the same filter"),
        (
            own_query("tag:'a'"),
            own_query("tag:'a'"),
            "b",
            "the same filter",
        ),
    ] {
        let first = in_context("a", "x", &first);
        let text = format!("{first}{}", in_context("b", "x", &second));
        let line = if refused == "a" {
            1
        } else {
            first.lines().count() + 1
        };
        assert_refused(&text, line, needle);
    }

    // A third context with the default is refused too.
    let text = format!(
        "{}{}{}",
        in_context("a", "x", &own_jql("project = A")),
        in_context("b", "x", &own_jql("project = B")),
        in_context("c", "x", JIRA),
    );
    assert_refused(&text, 15, "list_jql");
}

#[test]
fn two_contexts_on_other_sites_or_datadog_need_no_filter() {
    parse(&format!(
        "{}{}",
        in_context("a", "x", JIRA),
        in_context("b", "x", &with_site(JIRA, "company-b.atlassian.net")),
    ));
    parse(&format!(
        "{}{}",
        in_context("a", "x", FRESHSERVICE),
        in_context(
            "b",
            "x",
            &with_site(FRESHSERVICE, "company-b.freshservice.com")
        ),
    ));
    parse(&format!(
        "{}{}",
        in_context("a", "x", DATADOG),
        in_context("b", "x", DATADOG),
    ));
}

#[test]
fn a_connector_on_a_context_with_a_backup_needs_the_owner_to_allow_it() {
    let backup = |allows: &str| {
        format!("[contexts.a]\nstore.backup.remote = \"git@example.com:me/a.git\"\n{allows}\n")
    };
    for body in [DATADOG, JIRA, FRESHSERVICE] {
        // The remote, and not allowed: not said, or said false.
        for allows in ["", "store.backup.allows_client_data = false"] {
            let text = format!("{}{}", backup(allows), table("x", body));
            let line = backup(allows).lines().count() + 1;
            assert_refused(&text, line, "store.backup.allows_client_data = true");
        }
        // Allowed.
        parse(&format!(
            "{}{}",
            backup("store.backup.allows_client_data = true"),
            table("x", body)
        ));
        // No remote: no question.
        parse(&table("x", body));
    }
    // No connector: no question.
    parse(&backup(""));
}

#[test]
fn allows_client_data_is_read_and_needs_a_remote() {
    let config = parse(
        "[contexts.a]\nstore.backup.remote = \"r\"\nstore.backup.allows_client_data = true\n[contexts.b]\nstore.backup.remote = \"s\"\n",
    );
    let backup = |name: &str| {
        config.contexts[&name.parse::<ContextName>().unwrap()]
            .store
            .backup
            .clone()
            .unwrap()
    };
    assert!(backup("a").allows_client_data);
    assert!(!backup("b").allows_client_data);
    assert_refused(
        "[contexts.a]\nstore.backup.allows_client_data = true\n",
        2,
        "missing field `remote`",
    );
    assert_refused(
        "[contexts.a]\nstore.backup.remote = \"r\"\nstore.backup.allows_client_data = \"yes\"\n",
        3,
        "invalid type",
    );
}

#[test]
fn token_env_names_hold_the_connector_secrets() {
    let config = parse(&format!(
        "[forges.\"gitlab.com\"]\nkind = \"gitlab\"\ntoken_env = \"FORGE_TOKEN\"\n\n{}{}{}{}",
        in_context("a", "dd", DATADOG),
        in_context("a", "jira", JIRA),
        in_context("b", "fs", FRESHSERVICE),
        in_context(
            "b",
            "cmd",
            "kind = \"jira\"\nsite = \"company-b.atlassian.net\"\ntoken_cmd = \"print-token\"\nemail = \"me@company-b.example\"\n"
        ),
    ));
    assert_eq!(
        config.token_env_names().into_iter().collect::<Vec<_>>(),
        ["DD_API", "DD_APP", "FORGE_TOKEN", "FS_TOKEN", "JIRA_TOKEN"]
    );
}

#[test]
fn debug_does_not_print_the_commands() {
    let config = parse(FULL);
    let printed = format!("{config:?}");
    assert!(!printed.contains("op://"), "{printed}");
}

#[test]
fn debug_of_a_raw_table_does_not_print_the_commands() {
    let raw: RawConnector = toml::from_str(
        "kind = \"jira\"\nsite = \"company-a.atlassian.net\"\ntoken_cmd = \"op read op://secret\"\n",
    )
    .unwrap();
    let printed = format!("{raw:?}");
    assert!(!printed.contains("op://"), "{printed}");
}

#[test]
fn the_raw_tables_are_gone_after_parsing() {
    let config = parse(FULL);
    for context in config.contexts.values() {
        assert_eq!(context.connectors.len(), 3);
        assert!(context.connector_tables.is_empty());
    }
}

#[test]
fn no_refusal_prints_a_command() {
    let text = table(
        "x",
        "kind = \"jira\"\nsite = \"company-a.atlassian.net\"\nemail = \"me@company-a.example\"\ntoken_cmd = \"op read op://secret\"\ntoken_env = \"T\"\n",
    );
    let printed = refusal(&text).to_string();
    assert!(!printed.contains("op://"), "{printed}");
}
