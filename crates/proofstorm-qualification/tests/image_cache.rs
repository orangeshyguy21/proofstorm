//! Real distribution-registry round trip, opt-in because it needs Docker and
//! one anonymous upstream download. Normal workspace tests never pull images.
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use serde_json::json;
use sha2::{Digest, Sha256};

fn write(path: &Path, bytes: impl AsRef<[u8]>, executable: bool) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
    )
    .unwrap();
}

#[test]
fn invalid_or_incomplete_cache_stops_before_cases_and_cleans_its_registry() {
    for failure in [
        "none", "checksum", "plan", "manifest", "platform", "layer", "consumer",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let hash = |bytes: &[u8]| format!("sha256:{:x}", Sha256::digest(bytes));
        let config = serde_json::to_vec(
            &json!({"os":"linux","architecture":if failure=="platform" {"arm64"} else {"amd64"}}),
        )
        .unwrap();
        let manifest = serde_json::to_vec(&json!({"schemaVersion":2,"config":{"digest":hash(&config)},"layers":[{"digest":hash(b"layer")}]})).unwrap();
        let source = format!("docker.io/example/fixture@{}", hash(&manifest));
        let inputs = serde_json::to_vec(
            &json!({"format_version":1,"plan_digest":"fixture", "images":{source:["linux/amd64"]}}),
        )
        .unwrap();
        write(&root.join("bundle/registry/blob"), b"layer", false);
        write(&root.join("bundle/inputs.json"), &inputs, false);
        write(
            &root.join("bundle/SHA256SUMS"),
            format!(
                "{}  registry/blob\n{}  inputs.json\n",
                &hash(b"layer")[7..],
                &hash(&inputs)[7..]
            ),
            false,
        );
        write(
            &root.join("plan.json"),
            if failure == "plan" {
                b"{}".as_slice()
            } else {
                &inputs
            },
            false,
        );
        if failure == "checksum" {
            write(&root.join("bundle/registry/blob"), b"changed", false);
        }
        write(&root.join("manifest.json"), &manifest, false);
        write(&root.join("config.json"), &config, false);
        for (name, script) in [
            (
                "qualification-cache.sh",
                include_str!("../../../scripts/qualification-cache.sh"),
            ),
            (
                "qualification-docker.sh",
                include_str!("../../../scripts/qualification-docker.sh"),
            ),
        ] {
            write(&root.join("scripts").join(name), script, true);
        }
        write(
            &root.join("target/check/debug/proofstorm-qualification"),
            "#!/bin/sh\ntest \"$1\" = images || exit 1\ncat \"$2\"\n",
            true,
        );
        write(
            &root.join("stubs/timeout"),
            "#!/bin/sh\nshift\nexec \"$@\"\n",
            true,
        );
        write(&root.join("stubs/buildx"), "#!/bin/sh\nexit 1\n", true);
        write(
            &root.join("stubs/docker"),
            r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "$STUB_ROOT/commands"
case "$1 $2" in
  'context inspect') echo '{"Host":"unix:///fixture/docker.sock"}' ;;
  'info --format') printf '[{"Name":"buildx","Path":"%s/stubs/buildx"}]\n' "$STUB_ROOT" ;;
  'create --pull=never')
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == --name ]]; then printf '%s' "$2" > "$STUB_ROOT/owner"; break; fi
      shift
    done
    touch "$STUB_ROOT/created"; echo fixture-id ;;
  'start fixture-id') exit 0 ;;
  'inspect --format')
    case "$3" in
      *NetworkSettings*) echo '{"5000/tcp":[{"HostIp":"127.0.0.1","HostPort":"49150"}]}' ;;
      *) cat "$STUB_ROOT/owner" ;;
    esac ;;
  'buildx imagetools')
    [[ "$3 $4" == 'inspect --raw' && "$5" == 127.0.0.1:49150/* ]] || exit 92
    [[ "$FAILURE" != manifest ]] || exit 1
    cat "$STUB_ROOT/manifest.json" ;;
  'rm --force') [[ "$4" == fixture-id ]] || exit 1; touch "$STUB_ROOT/removed" ;;
  *) exit 92 ;;
esac
"#,
            true,
        );
        write(
            &root.join("stubs/curl"),
            r#"#!/bin/bash
set -euo pipefail
for arg in "$@"; do
  if [[ "$arg" == --head ]]; then [[ "$FAILURE" != layer ]]; exit; fi
done
case "${!#}" in
  http://127.0.0.1:49150/v2/) echo '{}' ;;
  http://127.0.0.1:49150/v2/images/*/blobs/*) cat "$STUB_ROOT/config.json" ;;
  *) exit 92 ;;
esac
"#,
            true,
        );
        write(
            &root.join("consumer.sh"),
            "#!/bin/sh\ntouch \"$STUB_ROOT/consumer\"\n[ \"$FAILURE\" != consumer ]\n",
            true,
        );
        let path = std::env::join_paths(
            std::iter::once(root.join("stubs"))
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let output = Command::new("bash")
            .arg(root.join("scripts/qualification-cache.sh"))
            .arg("with")
            .arg(root.join("plan.json"))
            .arg(root.join("bundle"))
            .arg(root.join("work"))
            .arg("bash")
            .arg(root.join("consumer.sh"))
            .env("PATH", path)
            .env("STUB_ROOT", &root)
            .env("FAILURE", failure)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            failure == "none",
            "{failure}: {output:?}"
        );
        assert_eq!(
            root.join("consumer").exists(),
            matches!(failure, "none" | "consumer"),
            "{failure}"
        );
        let created = root.join("created").exists();
        assert_eq!(
            created,
            !matches!(failure, "checksum" | "plan"),
            "{failure}"
        );
        assert_eq!(root.join("removed").exists(), created, "{failure}");
    }
}

#[test]
#[ignore = "needs local Docker, Buildx and anonymous upstream registry access"]
fn prepared_images_survive_upstream_loss_and_seed_fresh_registries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for (name, source) in [
        (
            "qualification-cache.sh",
            include_str!("../../../scripts/qualification-cache.sh"),
        ),
        (
            "qualification-docker.sh",
            include_str!("../../../scripts/qualification-docker.sh"),
        ),
        (
            "qualification-images.sh",
            include_str!("../../../scripts/qualification-images.sh"),
        ),
    ] {
        write(&root.join("scripts").join(name), source, true);
    }
    let source = "docker.io/library/busybox@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662";
    let logical = format!("proofstorm-registry.localhost:5000/upstream/{source}");
    let inputs = json!({"format_version":1,"plan_digest":"fixture", "images":{source:["linux/amd64","linux/arm64"]}});
    write(
        &root.join("plan.json"),
        serde_json::to_vec(&inputs).unwrap(),
        false,
    );
    // The separate CLI tests verify strict plan derivation; here the inventory
    // is deliberately small so this test checks transport rather than the catalog.
    write(
        &root.join("target/check/debug/proofstorm-qualification"),
        "#!/bin/sh\ntest \"$1\" = images || exit 1\ncat \"$2\"\n",
        true,
    );
    write(
        &root.join("seed.json"),
        serde_json::to_vec(&json!([{"image":logical,"source":source}])).unwrap(),
        false,
    );
    write(
        &root.join("stubs/timeout"),
        "#!/bin/sh\nexec perl -e 'alarm shift; exec @ARGV; die $!' \"$@\"\n",
        true,
    );
    let docker = Command::new("sh")
        .args(["-c", "command -v docker"])
        .output()
        .unwrap();
    assert!(docker.status.success());
    let docker = String::from_utf8(docker.stdout).unwrap().trim().to_owned();
    write(
        &root.join("stubs/docker"),
        r#"#!/bin/bash
set -euo pipefail
if [[ ${CACHE_OFFLINE:-} == true ]]; then
  for arg in "$@"; do
    case "$arg" in docker.io/*|ghcr.io/*) echo 'Unexpected upstream access' >&2; exit 92 ;; esac
  done
fi
exec "$REAL_DOCKER" "$@"
"#,
        true,
    );
    write(
        &root.join("consumer.sh"),
        r#"#!/bin/bash
set -Eeuo pipefail
root=$1
export CACHE_OFFLINE=true
source "$root/scripts/qualification-docker.sh"
sha=73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662
source="docker.io/library/busybox@sha256:$sha"
ref=$(qualification_cache_ref "$QUALIFICATION_IMAGE_CACHE" "$source")
arch=$(docker info --format '{{.Architecture}}')
case "$arch" in aarch64|arm64) platform=linux/arm64 ;; x86_64|amd64) platform=linux/amd64 ;; *) exit 1 ;; esac
# The restored cache cannot accept writes, including writes from a test.
status=$(curl -q --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' --request POST "http://$QUALIFICATION_IMAGE_CACHE/v2/forbidden/blobs/uploads/")
[[ "$status" == 405 ]]
# Both case attempts use fresh writable registries, and fail rather than fetch
# upstream if an entry is missing. Delete only the exact container we create.
for attempt in 1 2; do
  owner="cache-test-$(openssl rand -hex 12)"
  container=$(docker run --detach --pull=never --name "$owner" --label "cache-test=$owner" --publish 127.0.0.1::5000 registry:2)
  cleanup() {
    result=$?; trap - EXIT
    [[ "$(docker inspect --format '{{index .Config.Labels "cache-test"}}' "$container")" == "$owner" ]] || exit 1
    docker rm --force --volumes "$container" >/dev/null || exit 1
    exit "$result"
  }
  trap cleanup EXIT
  port=$(docker inspect --format '{{json .NetworkSettings.Ports}}' "$container" | jq -er '.["5000/tcp"][0].HostPort')
  destination="127.0.0.1:$port"
  for _ in {1..30}; do
    if curl -q --noproxy '*' --fail --silent "http://$destination/v2/" >/dev/null; then break; fi
    sleep 1
  done
  mkdir "$root/seed-$attempt"
  bash "$root/scripts/qualification-cache.sh" seed "$QUALIFICATION_IMAGE_CACHE" "$destination" "$root/seed.json" "$root/seed-$attempt"
  # Check actual bytes, not just the tag/index, in each freshly seeded registry.
  docker buildx imagetools inspect --raw "$destination/upstream/$source" > "$root/index-$attempt.json"
  manifest=$(jq -er --arg arch "${platform#linux/}" '.manifests[]|select(.platform.os=="linux" and .platform.architecture==$arch)|.digest' "$root/index-$attempt.json")
  docker buildx imagetools inspect --raw "${destination}/upstream/${source%@*}@$manifest" > "$root/manifest-$attempt.json"
  [[ "sha256:$(sha256sum "$root/manifest-$attempt.json" | cut -d' ' -f1)" == "$manifest" ]]
  while IFS= read -r blob; do
    curl -q --noproxy '*' --fail --silent --show-error "http://$destination/v2/upstream/${source%@*}/blobs/$blob" > "$root/blob"
    [[ "sha256:$(sha256sum "$root/blob" | cut -d' ' -f1)" == "$blob" ]]
  done < <(jq -er '.config.digest,.layers[].digest' "$root/manifest-$attempt.json")
  if [[ "$CACHE_NATIVE_PROBES" == true ]]; then
    docker pull --platform "$platform" "$destination/upstream/$source" >/dev/null
    docker run --rm --network none "$destination/upstream/$source" sh -c 'echo cached' > "$root/probe-$attempt"
    [[ "$(cat "$root/probe-$attempt")" == cached ]]
  fi
  docker rm --force --volumes "$container" >/dev/null
  trap - EXIT
done
if [[ "$CACHE_NATIVE_PROBES" == true ]]; then
  mkdir "$root/image-probe"
  jq -n --arg source "$source" --arg platform "$platform" '{platform:$platform,components:[{source:$source}],scenario:{kind:"image",component:{implementation:"workspace",version:"fixture",source:$source}}}' > "$root/case.json"
  bash "$root/scripts/qualification-images.sh" "$root/case.json" "$root/image-probe" "$QUALIFICATION_IMAGE_CACHE"
  jq -e --arg source "$source" 'keys==[$source]' "$root/image-probe/images.json"
fi
touch "$root/consumer-passed"
"#,
        true,
    );
    let path = std::env::join_paths(
        std::iter::once(root.join("stubs"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let cache = root.join("scripts/qualification-cache.sh");
    let run = |mode: &str, work: &str| {
        let mut command = Command::new("bash");
        command
            .arg("-x")
            .arg(&cache)
            .arg(mode)
            .arg(root.join("plan.json"))
            .arg(root.join("bundle"))
            .arg(root.join(work))
            .env("PATH", &path)
            .env(
                "CACHE_NATIVE_PROBES",
                if std::env::consts::OS == "linux" {
                    "true"
                } else {
                    "false"
                },
            )
            .env("REAL_DOCKER", &docker);
        if mode == "with" {
            command
                .arg("bash")
                .arg(root.join("consumer.sh"))
                .arg(&root)
                .env("CACHE_OFFLINE", "true");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run("prepare", "prepare");
    run("with", "restore");
    if std::env::consts::OS != "linux" {
        eprintln!(
            "Verified registry transfer and blob bytes; native daemon pulls/probes require Linux (Docker Desktop's daemon has a separate loopback)."
        );
    }
    assert!(root.join("consumer-passed").exists());
    // Artifact corruption must stop before starting the consumer.
    fs::remove_file(root.join("consumer-passed")).unwrap();
    write(&root.join("bundle/inputs.json"), b"changed", false);
    let output = Command::new("bash")
        .arg(&cache)
        .arg("with")
        .arg(root.join("plan.json"))
        .arg(root.join("bundle"))
        .arg(root.join("corrupt"))
        .arg("touch")
        .arg(root.join("consumer-passed"))
        .env("PATH", &path)
        .env("REAL_DOCKER", &docker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!root.join("consumer-passed").exists());
}
