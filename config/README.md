# Configuration examples

Add validated, runnable examples with the owning feature ticket rather than copying proposed settings into apparently supported configuration files. Early implementation types do not by themselves freeze a supported public configuration schema.

Worker process configuration, installed package metadata, and user-owned Runtime Profiles have different owners. Keep them separate. Examples use secret references, restricted local listeners, and explicit permission ceilings, never real credentials.

Client application configuration describes its Gateway connection and presentation choices. It does not grant Worker capabilities or replace execution policy. A different Runtime Profile does not require a new Worker Package.

`coding.toml` and `config/coding/` show the Coding Worker's operator-selected
workspace, repository, checks, prompts, agents, and skills. The example workspace
repository is operator-owned and is not committed; see the file header.
