#!/usr/bin/env bash
# Runs a CI template in a local emulator on a fixture repository whose second commit adds a copy of
# a function, with the release files served from the repository, and checks the reports.
#   test_ci_templates.sh gitlab|bitbucket MUSL_BINARY RUNNER   (RUNNER: gitlab-ci-local or pipeline-runner)
# Needs git, docker, python3 and the emulator. Run it in CI, not on a machine whose timings matter.
set -euo pipefail
kind=$1 bin=$(realpath "$2") runner=$3
top=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
repo=$work/repo
v=$(sed -n 's/^version = "\(.*\)"/\1/p' "$top/crates/scrutate/Cargo.toml" | head -1)
name=scrutate-$v-x86_64-unknown-linux-musl
g() { git -c user.name=t -c user.email=t@t -c commit.gpgsign=false "$@"; }

bash "$top/integrations/fixture.sh" "$repo"
cd "$repo"
g init -q -b main
# long enough for `dupes --min-tokens 60` and for the review's duplicate search
orders() {
  cat << EOF
def $1(orders, region_table, discount_rules):
    total_amount = 0
    shipped_count = 0
    for order in orders:
        region_rate = region_table.lookup(order.region_code, order.created_date)
        discount_value = discount_rules.best_match(order.customer_group, order.amount)
        if order.status == "shipped" and order.amount > discount_value:
            total_amount += order.amount * region_rate - discount_value
            shipped_count += 1
        elif order.status == "returned":
            total_amount -= order.refund_amount * region_rate
        else:
            total_amount += order.pending_amount * region_rate
    average_amount = total_amount / max(shipped_count, 1)
    return {"total": total_amount, "shipped": shipped_count, "average": average_amount}
EOF
}
orders summarize_orders > src/orders.py
# the release as release.yml packs it; committed, since both emulators copy only tracked files
mkdir -p "release/$name"
cp "$bin" "release/$name/scrutate"
chmod +x "release/$name/scrutate"
(cd release && tar czf "$name.tar.gz" "$name" && rm -r "$name" && sha256sum "$name.tar.gz" > "$name.sha256")

case $kind in
  gitlab)
    cp "$top/integrations/gitlab/.gitlab-ci.yml" .gitlab-ci.yml
    # gitlab-ci-local copies back only artifacts:paths, not reports: list the report there in this copy
    python3 - << 'EOF'
p = ".gitlab-ci.yml"
s = open(p).read()
old = "paths: [scrutate-dupes.json, scrutate-todos.jsonl]"
assert s.count(old) == 1, "the template's artifacts line changed"
open(p, "w").write(s.replace(old, "paths: [scrutate-dupes.json, scrutate-todos.jsonl, gl-code-quality-report.json]"))
EOF
    ;;
  bitbucket) cp "$top/integrations/bitbucket/bitbucket-pipelines.yml" . ;;
  *) echo "unknown template: $kind"; exit 2 ;;
esac
g add -A
g commit -qm base
base=$(git rev-parse HEAD)
orders summarize_orders_again > src/orders_copy.py
g add -A
g commit -qm "a copy of summarize_orders"

case $kind in
  gitlab)
    # the variables a merge request pipeline has; $CI_PROJECT_DIR is expanded by the emulator
    # shellcheck disable=SC2016
    "$runner" --variable CI_PIPELINE_SOURCE=merge_request_event --variable "CI_MERGE_REQUEST_DIFF_BASE_SHA=$base" \
      --variable 'SCRUTATE_URL=file://$CI_PROJECT_DIR/release' scrutate
    out=$repo
    ;;
  bitbucket)
    # the clone directory pipeline-runner uses, as Bitbucket does
    echo "SCRUTATE_URL=file:///opt/atlassian/pipelines/agent/build/release" > .env
    export XDG_DATA_HOME=$work/data
    "$runner" run default
    out=$(dirname "$(find "$XDG_DATA_HOME" -name scrutate-dupes.json | head -1)")
    ;;
esac

python3 - "$kind" "$out" << 'EOF'
import json, os, sys
kind, out = sys.argv[1], sys.argv[2]
dupes = json.load(open(os.path.join(out, "scrutate-dupes.json")))
text = json.dumps(dupes)
assert "src/orders.py" in text and "src/orders_copy.py" in text, f"dupes lacks the copy: {text[:2000]}"
print("dupes lists src/orders.py and src/orders_copy.py")
if kind == "gitlab":
    assert os.path.exists(os.path.join(out, "scrutate-todos.jsonl")), "no scrutate-todos.jsonl"
    report = json.load(open(os.path.join(out, "gl-code-quality-report.json")))
    print(json.dumps(report, indent=1))
    assert isinstance(report, list) and report, "the Code Quality report is not a non-empty list"
    for i in report:
        for k in ("description", "check_name", "fingerprint", "severity"):
            assert isinstance(i.get(k), str) and i[k], f"{k} missing in {i}"
        assert isinstance(i["location"]["path"], str) and i["location"]["lines"]["begin"] >= 1, i
    assert any(i["check_name"] == "scrutate-duplicate" and i["location"]["path"] == "src/orders_copy.py" for i in report), \
        "the report has no duplicate issue at src/orders_copy.py"
    print("the Code Quality report lists the new duplicate at src/orders_copy.py")
EOF
