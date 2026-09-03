#!/bin/sh
set -eu

config_file=${CDK_MINTD_CONFIG_FILE:-/config.toml}
work_dir=${CDK_MINTD_WORK_DIR:-/app/data}
init_mode=${CDK_MINTD_INIT_MODE:-}

# CDK 0.18 stores the authoritative document in the mint database. Import it
# exactly once; later restarts never apply file changes implicitly.
if ! cdk-mintd --work-dir "$work_dir" config show >/dev/null 2>&1; then
    case "$init_mode" in
        new)
            init_flag=--new-mint
            ;;
        existing)
            init_flag=--existing-mint
            ;;
        "")
            echo "CDK configuration is absent; set CDK_MINTD_INIT_MODE=new or existing" >&2
            exit 1
            ;;
        *)
            echo "invalid CDK_MINTD_INIT_MODE '$init_mode'; expected new or existing" >&2
            exit 1
            ;;
    esac
    cdk-mintd config validate --file "$config_file"
    cdk-mintd --work-dir "$work_dir" config init "$init_flag" --file "$config_file"
fi

exec cdk-mintd --work-dir "$work_dir"
