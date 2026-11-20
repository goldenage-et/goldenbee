pub(crate) fn help() {
    println!(
        "Usage: gb-client [command] [options]\n\n\
         Gateway profiles:\n  \
         gateway|gw add <name> <url> <token>   register or update a Gateway\n  \
         gateway|gw switch <name>              select the active Gateway\n  \
         gateway|gw open <name>                open an interactive session\n  \
         gateway|gw list [--json]              list stored Gateways\n  \
         gateway|gw detail <name> [--json]     show one Gateway\n\n\
         Session:\n  \
         chat [--worker <id>] [--agent <name>] open the active Gateway in a chat\n  \
         gb-client                            alias for `chat`\n  \
         manage                                open Worker, Provider, and Model management\n\n\
         Gateway operations:\n  \
         provider list [--json]\n  \
         provider show <id> [--json]\n  \
         provider add <id> --adapter <kind> [--model <id>] [--endpoint <url>] [--modality <text>] [--allowed-model <id>] [--auto-default] [--tool-calling] [--context-window <n>] [--local] [--residency <label>] (--worker <id>... | --all-workers) [--json]\n  \
         provider update <id> --revision <n> [metadata flags] [--enable|--disable] [--no-auto-default] [--no-tool-calling] [--remote] [--clear-allowed-models] [--clear-workers] [--json]\n  \
         provider discover <id> [--json]\n  \
         provider credential set <id>          read the provider key from stdin\n  \
         provider credential revoke <id> [--json]\n  \
         provider pair --worker <id> --instance <name> [--json]\n  \
         model list [--provider <id>] [--json]\n  \
         model allow <provider> --model <id>... [--json]\n  \
         model clear <provider> [--json]\n  \
         model preferred <provider> --model <id> [--json]\n  \
         model discover <provider> [--json]\n  \
         health\n  \
         workers [--capability <id>] [--protocol <version>] [--locality local|remote] [--health healthy|unreachable|any] [--json]\n  \
         package install --source <folder|url> [--package <name>] [--digest <sha256:...>] [--json]\n  \
         package list [--json]\n  \
         instance deploy --source <folder|url> --instance <id> [--package <name>] [--digest <sha256:...>] [--worker <id>] [--operation <id>] [--grant <scope>]... [--profile <name>] [--profile-create] [--start] [--json]\n  \
         instance list [--json]\n  \
         instance inspect <instance> [--json]\n  \
         instance start|stop|restart <instance> [--json]\n  \
         instance logs <instance> [--tail <n>] [--json]\n  \
         instance upgrade <instance> --operation <id> --release <digest> [--authorize <permission>]... [--work-policy drain|pin|migrate] [--verify-input <text>] [--json]\n  \
         instance rollback <instance> --operation <id> [--json]\n  \
         worker list [--all] [--json]\n  \
         worker show <id> [--json]\n  \
         worker add <id> <endpoint> [--instance <id>] [--package <name>] [--capability <id>]... [--control-token-stdin] [--no-preflight] [--json]\n  \
         worker update <id> --revision <n> [--endpoint <url>] [--instance <id>] [--package <name>] [--capability <id>]... [--json]\n  \
         worker disable|enable|probe|remove <id>\n  \
         worker audit [--worker <id>] [--json]\n  \
         diagnose [task-id] --worker <id> [--json]\n  \
         team list [--json]\n  \
         team show <name> [--json]\n  \
         team create <name> --master <worker> [--member <worker[:capability[,capability]]>]... [--max-depth <n>] [--max-fanout <n>] [--locality local|remote] [--json]\n  \
         team revise <name> --version <n> --master <worker> [--member <worker[:capability[,capability]]>]... [--max-depth <n>] [--max-fanout <n>] [--locality local|remote] [--json]\n  \
         team validate|activate|deactivate <name> [--version <n>] [--json]\n  \
         profile list --worker <id> [--json]\n  \
         profile show <name> --worker <id> [--json]\n  \
         profile create <name> --worker <id> --instruction <text> [--model <id>] [--capability <id>]... [--grant <scope>]... [--restriction <scope>]... [--composition <kind>] [--member <agent>]... [--skill <name>]... [--mcp <connection>]... [--secret-ref <reference>]... [--compaction-budget <tokens>] [--json]\n  \
         profile revise <name> --worker <id> --expected <n> --instruction <text> [profile options] [--json]\n  \
         profile validate <name> --worker <id> [--revision <n>] [--json]\n  \
         profile activate <name> --worker <id> --revision <n> [--json]\n  \
         profile deactivate <name> --worker <id> [--json]\n  \
         submit --worker <id> --agent <name> --input <text> [--session <id>] [--profile <name>] [--team <name>] [--team-version <n>] [--key <key>] [--json]\n  \
         task <id> [--json]\n  \
         cancel <id> [--json]\n  \
         events <id> [--after <seq>] [--limit <n>] [--json]\n  \
         watch <id> [--after <seq>] [--json]\n  \
         artifact list <task-id> [--json]\n  \
         artifact show <task-id> <artifact-id> [--json]\n  \
         artifact download <task-id> <artifact-id> --output <path> [--force]\n  \
         fleet hosts [--json]\n  \
         fleet host <host> [--json]\n  \
         fleet provision <host> --operation <id> --instance <id> --worker <id> --release <digest> [--addr <address>] [--config <path>] [--workspace <path>] [--read-only] [--grant <scope>]... [--secret-ref <ref>]... [--profile-name <name> [--profile-create] [--profile-instruction <text>] [--profile-member <agent>]... [--profile-capability <id>]... [--profile-grant <scope>]...] [--control-token <token>] [--json]\n  \
         fleet inspect <host> <instance> [--json]\n  \
         fleet upgrade <host> <instance> --operation <id> --release <digest> [--authorize <permission>]... [--work-policy drain|pin|migrate] [--verify-input <text>] [--json]\n  \
         fleet rollback <host> <instance> --operation <id> [--json]\n  \
         approvals <task-id> [--pending] [--json]\n  \
         approve <task-id> <approval-id> [--digest <sha256:...>] [--json]\n  \
         reject <task-id> <approval-id> [--digest <sha256:...>] [--json]\n  \
         revoke <task-id> <approval-id> [--json]\n  \
         control acquire --worker <id> --session <id> [--ttl <ms>] [--json]\n  \
         control status --worker <id> --session <id> [--json]\n  \
         control command --worker <id> --session <id> --generation <n> --epoch <epoch> (--navigate <url>|--click <selector>|--type <selector> --text <text>|--extract <selector>) [--json]\n  \
         control handback --worker <id> --session <id> --generation <n> --epoch <epoch> [--json]\n  \
         control audit --worker <id> --session <id> [--json]\n  \
         --version\n\n\
         Options: --gateway <url>, --token <token>, --worker <id>, --agent <name>.\n\
         Without flags, commands use the active profile. Environment fallbacks:\n\
         GOLDENBEE_GATEWAY_URL, GOLDENBEE_TOKEN, GOLDENBEE_CLI_HOME.\n\
         Errors print a stable code. Exit codes: 0 success, 1 operation error,\n\
         2 usage, 3 validation or revision conflict, 4 authorization, 5 transport\n\
         uncertainty such as a lost acknowledgment."
    );
}
