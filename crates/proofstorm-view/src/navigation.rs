//! Browser routes shared with the HTTP server's app-shell fallback.
use std::fmt::Write;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AppRoute {
    #[default]
    Home,
    System,
    Catalog,
    Builds,
    Cell {
        id: String,
        component: String,
    },
}

impl AppRoute {
    #[must_use]
    pub fn parse(path: &str) -> Option<Self> {
        if path == "/" {
            return Some(Self::Home);
        }
        let parts: Vec<_> = path
            .strip_prefix('/')?
            .trim_end_matches('/')
            .split('/')
            .collect();
        match parts.as_slice() {
            ["system"] => Some(Self::System),
            ["catalog"] => Some(Self::Catalog),
            ["catalog", "builds"] => Some(Self::Builds),
            ["cells", id] => Some(Self::Cell {
                id: decode(id)?,
                component: String::new(),
            }),
            ["cells", id, "components", component] => Some(Self::Cell {
                id: decode(id)?,
                component: decode(component)?,
            }),
            _ => None,
        }
    }

    #[must_use]
    pub fn path(&self) -> String {
        match self {
            Self::Home => "/".into(),
            Self::System => "/system".into(),
            Self::Catalog => "/catalog".into(),
            Self::Builds => "/catalog/builds".into(),
            Self::Cell { id, component } => {
                let mut path = format!("/cells/{}", encode(id));
                if !component.is_empty() {
                    path.push_str("/components/");
                    path.push_str(&encode(component));
                }
                path
            }
        }
    }
}

fn encode(value: &str) -> String {
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            result.push(char::from(byte));
        } else {
            write!(result, "%{byte:02X}").expect("writing to a string");
        }
    }
    result
}

fn decode(value: &str) -> Option<String> {
    let mut result = Vec::new();
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        result.push(if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            u8::try_from(high * 16 + low).ok()?
        } else {
            byte
        });
    }
    let result = String::from_utf8(result).ok()?;
    (!result.is_empty() && result != "." && result != ".." && !result.chars().any(char::is_control))
        .then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_and_component_links_round_trip() {
        for route in [
            AppRoute::Home,
            AppRoute::System,
            AppRoute::Catalog,
            AppRoute::Builds,
            AppRoute::Cell {
                id: "cell-123".into(),
                component: String::new(),
            },
            AppRoute::Cell {
                id: "cell +雪".into(),
                component: "mint/db #1?%".into(),
            },
        ] {
            assert_eq!(AppRoute::parse(&route.path()), Some(route));
        }
        assert_eq!(AppRoute::parse("/catalog/"), Some(AppRoute::Catalog));
        assert_eq!(
            AppRoute::parse("/cells/a/components/mint%2Fdb")
                .unwrap()
                .path(),
            "/cells/a/components/mint%2Fdb"
        );
    }

    #[test]
    fn api_assets_and_malformed_links_are_not_app_routes() {
        for path in [
            "/v1/missing",
            "/missing.js",
            "/unknown",
            "/cells/",
            "/cells/a/components/",
            "/cells/a/other",
            "/cells/%",
            "/cells/%GG",
            "/cells/%FF",
            "/cells/%00",
            "/cells/%2e%2e",
            "//catalog",
        ] {
            assert_eq!(AppRoute::parse(path), None, "{path}");
        }
    }
}
