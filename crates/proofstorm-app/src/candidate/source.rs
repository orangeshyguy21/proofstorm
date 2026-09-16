use crate::Error;
use proofstorm_core::CandidateInput;
use serde_json::Value;

pub(super) async fn resolve(
    source: &CandidateInput,
    repository: &str,
) -> Result<(String, String), Error> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("proofstorm/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Error::failure(e.to_string(), None))?;
    resolve_with(source, repository, |path| {
        let client = client.clone();
        let repository = repository.to_owned();
        async move { get(&client, &repository, &path).await }
    })
    .await
}

async fn resolve_with<F, Fut>(
    source: &CandidateInput,
    repository: &str,
    get: F,
) -> Result<(String, String), Error>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Value, Error>>,
{
    match source {
        CandidateInput::Commit {
            sha: Some(sha),
            url: None,
        } => Ok((format!("https://github.com/{repository}.git"), sha.clone())),
        CandidateInput::PullRequest { url } => {
            let number = url
                .rsplit('/')
                .next()
                .ok_or_else(|| Error::problem("candidate_source_invalid", "Missing PR number"))?;
            let response = get(vec!["pulls".into(), number.into()]).await?;
            if response["base"]["repo"]["full_name"].as_str() != Some(repository)
                || response["base"]["repo"]["private"] != false
                || response["head"]["repo"]["private"] != false
            {
                return Err(Error::problem(
                    "candidate_repository_mismatch",
                    "PR must target the canonical public repository and have a public head repository",
                ));
            }
            let head = response["head"]["repo"]["full_name"]
                .as_str()
                .filter(|name| {
                    name.split('/').count() == 2
                        && name
                            .split('/')
                            .all(|part| !part.is_empty() && part != "." && part != "..")
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
                })
                .ok_or_else(|| {
                    Error::problem("candidate_source_invalid", "Invalid public head repository")
                })?;
            Ok((
                format!("https://github.com/{head}.git"),
                sha(&response["head"]["sha"])?,
            ))
        }
        CandidateInput::Tag { tag } => {
            let response =
                get(vec!["git".into(), "ref".into(), "tags".into(), tag.clone()]).await?;
            let mut object = response["object"].clone();
            for _ in 0..8 {
                let id = sha(&object["sha"])?;
                match object["type"].as_str() {
                    Some("commit") => {
                        return Ok((format!("https://github.com/{repository}.git"), id));
                    }
                    Some("tag") => {
                        object =
                            get(vec!["git".into(), "tags".into(), id]).await?["object"].clone();
                    }
                    _ => {
                        return Err(Error::problem(
                            "candidate_tag_invalid",
                            "Tag must resolve to a commit",
                        ));
                    }
                }
            }
            Err(Error::problem(
                "candidate_tag_invalid",
                "Annotated tag nesting exceeds eight objects",
            ))
        }
        CandidateInput::Commit { .. } => Err(Error::problem(
            "candidate_source_invalid",
            "Source was not normalized",
        )),
    }
}

fn sha(value: &Value) -> Result<String, Error> {
    value
        .as_str()
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| {
            Error::problem(
                "candidate_source_invalid",
                "GitHub did not return a full commit SHA",
            )
        })
}

async fn get(client: &reqwest::Client, repository: &str, path: &[String]) -> Result<Value, Error> {
    let mut url = reqwest::Url::parse(&format!("https://api.github.com/repos/{repository}/"))
        .map_err(|e| Error::problem("candidate_source_invalid", e.to_string()))?;
    url.path_segments_mut()
        .map_err(|()| Error::problem("candidate_source_invalid", "Invalid repository"))?
        .pop_if_empty()
        .extend(path);
    // Anonymous reads intentionally cannot resolve private sources through ambient credentials.
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::problem("candidate_source_resolution_failed", e.to_string()))?;
    if !response.status().is_success() {
        return Err(Error::problem(
            "candidate_source_resolution_failed",
            format!("GitHub returned {}", response.status()),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Error::problem("candidate_source_resolution_failed", e.to_string()))?
    {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err(Error::problem(
                "candidate_source_resolution_failed",
                "GitHub response exceeded 1 MiB",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::problem("candidate_source_resolution_failed", e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    #[tokio::test]
    async fn candidate_pr_resolves_public_fork_head_and_rejects_private_or_foreign_sources() {
        let source = CandidateInput::PullRequest {
            url: "https://github.com/cashubtc/cdk/pull/123".into(),
        };
        let public = json!({"base":{"repo":{"full_name":"cashubtc/cdk","private":false}},"head":{"sha":"B".repeat(40),"repo":{"full_name":"contributor/cdk","private":false}}});
        let resolved = resolve_with(&source, "cashubtc/cdk", |path| {
            assert_eq!(path, ["pulls", "123"]);
            std::future::ready(Ok(public.clone()))
        })
        .await
        .unwrap();
        assert_eq!(
            resolved,
            (
                "https://github.com/contributor/cdk.git".into(),
                "b".repeat(40)
            )
        );
        for (path, value) in [
            ("/base/repo/full_name", json!("another/repo")),
            ("/head/repo/private", json!(true)),
            ("/head/repo/full_name", json!("../cdk")),
            ("/head/sha", json!("short")),
        ] {
            let mut invalid = public.clone();
            *invalid.pointer_mut(path).unwrap() = value;
            assert!(
                resolve_with(&source, "cashubtc/cdk", |_| std::future::ready(Ok(
                    invalid.clone()
                )))
                .await
                .is_err()
            );
        }
    }

    #[tokio::test]
    async fn candidate_tags_peel_nested_objects_and_fail_closed_on_missing_or_noncommit_objects() {
        let source = CandidateInput::Tag {
            tag: "release/v1".into(),
        };
        let paths = Mutex::new(Vec::new());
        let responses = Mutex::new(std::collections::VecDeque::from([
            json!({"object":{"type":"tag","sha":"a".repeat(40)}}),
            json!({"object":{"type":"tag","sha":"b".repeat(40)}}),
            json!({"object":{"type":"commit","sha":"c".repeat(40)}}),
        ]));
        let result = resolve_with(&source, "cashubtc/cdk", |path| {
            paths.lock().unwrap().push(path);
            std::future::ready(Ok(responses.lock().unwrap().pop_front().unwrap()))
        })
        .await
        .unwrap();
        assert_eq!(result.1, "c".repeat(40));
        assert_eq!(
            paths.lock().unwrap()[0],
            ["git", "ref", "tags", "release/v1"]
        );
        assert_eq!(paths.lock().unwrap().len(), 3);
        for value in [
            json!({}),
            json!({"object":{"type":"tree","sha":"a".repeat(40)}}),
            json!({"object":{"type":"tag","sha":"a".repeat(40)}}),
        ] {
            assert!(
                resolve_with(&source, "cashubtc/cdk", |_| std::future::ready(Ok(
                    value.clone()
                )))
                .await
                .is_err()
            );
        }
        assert!(
            resolve_with(&source, "cashubtc/cdk", |_| std::future::ready(Err(
                Error::problem("candidate_source_resolution_failed", "GitHub returned 404")
            )))
            .await
            .is_err()
        );
    }
}
