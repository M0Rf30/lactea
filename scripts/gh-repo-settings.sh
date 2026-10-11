#!/bin/sh
# Apply the GitHub repository settings Lactea expects (needs `gh` with admin rights).
# Branch protection and auto-merge need a public repository (or GitHub Pro for private ones).
set -eu
REPO="${1:-M0Rf30/lactea}"

gh api -X PATCH "repos/$REPO" \
  -F allow_auto_merge=true -F allow_squash_merge=true -F allow_merge_commit=false \
  -F allow_rebase_merge=true -F delete_branch_on_merge=true \
  -f squash_merge_commit_title=PR_TITLE -f squash_merge_commit_message=PR_BODY >/dev/null

# release-plz and the dependabot auto-merge workflow need write permissions and PR approval.
gh api -X PUT "repos/$REPO/actions/permissions/workflow" \
  -f default_workflow_permissions=write -F can_approve_pull_request_reviews=true

# Required checks: job names from .github/workflows/{ci,commits}.yml. gpu-tests and eel-oracle stay advisory.
gh api -X PUT "repos/$REPO/branches/main/protection" --input - <<'EOF'
{
  "required_status_checks": {
    "strict": false,
    "contexts": ["fmt", "clippy", "test (no GPU, no oracle, no corpus)", "msrv", "deny", "package",
                 "wasm", "actionlint", "scripts", "title", "commits"]
  },
  "enforce_admins": false,
  "required_pull_request_reviews": null,
  "restrictions": null
}
EOF
echo "settings applied to $REPO"
