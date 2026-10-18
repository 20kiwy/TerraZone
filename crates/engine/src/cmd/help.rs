//! Help → community and project links (TerraZone's own community hub, website and source
//! repository). The commands return the URL; the frontend opens it.

use serde_json::{Value, json};

use super::*;

/// TerraZone's repository id on GitHub.
pub const APP_ID: &str = "terrazon";
/// TerraZone's community hub (the `discord` key keeps its name for compatibility).
pub const DISCORD_URL: &str = "https://terrazon.app/community";
/// TerraZone's website.
pub const WEBSITE_URL: &str = "https://terrazon.app";

/// TerraZone's project page on GitHub (releases, issues, source).
pub fn app_page_url() -> String {
    github_url()
}

/// TerraZone's source repository.
pub fn github_url() -> String {
    format!("https://github.com/terrazon-app/{APP_ID}")
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "help.discord", "TerraZone Community", ["Help"], None, "{} → {url} the TerraZone community hub", always, |_, _| url(DISCORD_URL.into())),
        cmd!(query "help.website", "TerraZone Website", ["Help"], None, "{} → {url} the project's home page", always, |_, _| url(WEBSITE_URL.into())),
        cmd!(query "help.appPage", "TerraZone on GitHub", ["Help"], None, "{} → {url} the project's page on GitHub", always, |_, _| url(app_page_url())),
        cmd!(query "help.github", "TerraZone on GitHub", ["Help"], None, "{} → {url} source code, issues and releases", always, |_, _| url(github_url())),
        cmd!(query "help.links", "Links", [], None, "{} → {discord, website, appPage, github}", always, |_, _| {
            Ok(json!({ "discord": DISCORD_URL, "website": WEBSITE_URL, "appPage": app_page_url(), "github": github_url() }))
        }),
    ]
}

fn url(u: String) -> Result<Value> {
    Ok(json!({ "url": u }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        let mut s = Session::new();
        assert_eq!(s.execute("help.discord", &json!({})).unwrap()["url"], "https://terrazon.app/community");
        assert_eq!(s.execute("help.appPage", &json!({})).unwrap()["url"], "https://github.com/terrazon-app/terrazon");
        assert_eq!(s.execute("help.github", &json!({})).unwrap()["url"], "https://github.com/terrazon-app/terrazon");
        assert_eq!(s.execute("help.links", &json!({})).unwrap()["website"], "https://terrazon.app");
    }
}
