#!/bin/sh
# Create an offline, disposable AgenTuna performance fixture.
# Usage: sh scripts/perf-fixture.sh NEW_TEMP_DIR [SKILLS=2000] [SERVERS=200]
# Skills are divided between User and Project stores and linked to each agent.
# Server names are shared across all three agents; Codex has no project MCP path.
set -eu

if [ "$#" -lt 1 ] || [ "$#" -gt 3 ]; then
    printf 'usage: %s NEW_TEMP_DIR [SKILLS=2000] [SERVERS=200]\n' "$0" >&2
    exit 2
fi

dest=$1
skills=${2:-2000}
servers=${3:-200}
case $dest in /*) ;; *) printf 'fixture path must be absolute\n' >&2; exit 2 ;; esac
for number in "$skills" "$servers"; do
    case $number in ''|*[!0-9]*) printf 'counts must be non-negative integers\n' >&2; exit 2 ;; esac
done
# Strip leading zeroes before shell arithmetic, which may otherwise interpret them as octal.
while [ "${skills#0}" != "$skills" ]; do skills=${skills#0}; done
while [ "${servers#0}" != "$servers" ]; do servers=${servers#0}; done
skills=${skills:-0}
servers=${servers:-0}
if [ "${#skills}" -gt 5 ] || [ "${#servers}" -gt 5 ] || [ "$skills" -gt 20000 ] || [ "$servers" -gt 20000 ]; then
    printf 'each count must be at most 20000\n' >&2
    exit 2
fi

parent=$(CDPATH='' cd -P "$(dirname "$dest")" && pwd -P)
leaf=$(basename "$dest")
case $leaf in ''|.|..) printf 'invalid fixture directory name\n' >&2; exit 2 ;; esac
system_tmp=$(CDPATH='' cd -P /tmp && pwd -P)
case $parent/ in
    "$system_tmp"/*) ;;
    *)
        if [ -z "${TMPDIR:-}" ] || [ ! -d "$TMPDIR" ]; then
            printf 'fixture must be under /tmp or TMPDIR\n' >&2; exit 2
        fi
        temp_root=$(CDPATH='' cd -P "$TMPDIR" && pwd -P)
        case $parent/ in "$temp_root"/*) ;; *) printf 'fixture must be under /tmp or TMPDIR\n' >&2; exit 2 ;; esac
        ;;
esac
root=$parent/$leaf
# Never modify or remove an existing directory, including through a symlink.
if ! mkdir "$root"; then
    printf 'fixture destination must not already exist: %s\n' "$root" >&2
    exit 2
fi

home=$root/home
project=$root/project
user_store=$home/.agentuna
project_store=$project/.agentuna
mkdir -p "$root/support" "$root/global" "$root/tmp" "$root/config" "$root/data" "$root/cache" \
    "$user_store/skills" "$project_store/skills" \
    "$home/.claude/skills" "$home/.codex/skills" "$home/.omp/agent/managed-skills" \
    "$project/.claude/skills" "$project/.agents/skills"

user_manifest=$user_store/store.json
project_manifest=$project_store/store.json
printf '{"schema_version":1,"store_id":"00000000-0000-7000-8000-000000000001","scope":"user","updated_at":"2026-01-01T00:00:00Z","items":[\n' > "$user_manifest"
printf '{"schema_version":1,"store_id":"00000000-0000-7000-8000-000000000002","scope":"project","updated_at":"2026-01-01T00:00:00Z","items":[\n' > "$project_manifest"
user_sep=
project_sep=
i=1
while [ "$i" -le "$skills" ]; do
    slug=$(printf 'perf-skill-%06d' "$i")
    id=$(printf 'itm_00000000-0000-7000-8000-%012d' "$i")
    if [ $((i % 2)) -eq 1 ]; then
        store=$user_store
        manifest=$user_manifest
        separator=$user_sep
        user_sep=,
    else
        store=$project_store
        manifest=$project_manifest
        separator=$project_sep
        project_sep=,
    fi
    mkdir "$store/skills/$slug"
    # Alphabetically sorted frontmatter is already canonical for skill hashing.
    printf '%s\n' '---' 'description: Synthetic offline performance fixture' "name: $slug" '---' '' "# $slug" '' 'Local fixture; no external services.' > "$store/skills/$slug/SKILL.md"
    size=$(wc -c < "$store/skills/$slug/SKILL.md")
    size=$((size))
    digest=$(openssl dgst -sha256 "$store/skills/$slug/SKILL.md")
    digest=${digest##* }
    hash=$(printf 'SKILL.md\0%s\0%s\n' "$size" "$digest" | openssl dgst -sha256)
    hash=${hash##* }
    printf '%s{"id":"%s","kind":"skill","slug":"%s","path":"skills/%s","hash":"%s","name":"%s"}\n' "$separator" "$id" "$slug" "$slug" "$hash" "$slug" >> "$manifest"
    if [ $((i % 2)) -eq 1 ]; then
        ln -s "../../.agentuna/skills/$slug" "$home/.claude/skills/$slug"
        ln -s "../../.agentuna/skills/$slug" "$home/.codex/skills/$slug"
        ln -s "../../../.agentuna/skills/$slug" "$home/.omp/agent/managed-skills/$slug"
    else
        ln -s "../../.agentuna/skills/$slug" "$project/.claude/skills/$slug"
        # Codex and omp really share this one project discovery root.
        ln -s "../../.agentuna/skills/$slug" "$project/.agents/skills/$slug"
    fi
    i=$((i + 1))
done
printf '],"disabled":[]}\n' >> "$user_manifest"
printf '],"disabled":[]}\n' >> "$project_manifest"

claude_user=$home/.claude.json
claude_project=$project/.mcp.json
omp_user=$home/.omp/agent/mcp.json
omp_project=$project/mcp.json
codex=$home/.codex/config.toml
for file in "$claude_user" "$claude_project" "$omp_user" "$omp_project"; do printf '{"mcpServers":{\n' > "$file"; done
: > "$codex"
user_sep=
project_sep=
i=1
while [ "$i" -le "$servers" ]; do
    name=$(printf 'perf-server-%06d' "$i")
    if [ $((i % 2)) -eq 1 ]; then
        claude=$claude_user; omp=$omp_user; separator=$user_sep; user_sep=,
    else
        claude=$claude_project; omp=$omp_project; separator=$project_sep; project_sep=,
    fi
    # Definitions are inert and require explicit consent before an app can run them.
    for file in "$claude" "$omp"; do
        printf '%s"%s":{"command":"/usr/bin/false","args":[]}\n' "$separator" "$name" >> "$file"
    done
    printf '[mcp_servers.%s]\ncommand = "/usr/bin/false"\nargs = []\n\n' "$name" >> "$codex"
    i=$((i + 1))
done
for file in "$claude_user" "$claude_project" "$omp_user" "$omp_project"; do printf '}}\n' >> "$file"; done
printf '# Synthetic User Claude rules\n' > "$home/.claude/CLAUDE.md"
printf '# Synthetic User Codex rules\n' > "$home/.codex/AGENTS.md"
printf '# Synthetic User omp rules\n' > "$home/.omp/agent/AGENTS.md"
printf '# Synthetic Project Claude rules\n' > "$project/CLAUDE.md"
printf '# Synthetic Project agent rules\n' > "$project/AGENTS.md"
printf 'Fixture: %s\nSkills: %s (User %s, Project %s)\nDistinct MCP names: %s (each agent)\n' \
    "$root" "$skills" "$(((skills + 1) / 2))" "$((skills / 2))" "$servers"
