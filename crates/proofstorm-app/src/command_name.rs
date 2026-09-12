//! Only presentation uses this hint; installation and authorization never depend on it.
#[must_use]
pub fn command_name() -> &'static str {
    match std::env::var("PROOFSTORM_CLI_NAME").as_deref() {
        Ok("storm") => "storm",
        Ok("proofstorm") => "proofstorm",
        _ if std::env::args_os()
            .next()
            .as_deref()
            .and_then(|path| std::path::Path::new(path).file_name())
            .is_some_and(|name| name == "storm") =>
        {
            "storm"
        }
        _ => "proofstorm",
    }
}
