# Systems and paths

Record repositories, branches, environments, machines/services, local and remote paths, caches,
datasets, tracking identities, launch/resume rules, and source-of-truth ownership. Flag live state.

## Local source of truth

- Project and Git root: `/home/rachitmehta/Desktop/CrispPDF`.
- Rust source: `src/`.
- Desktop integration: `resources/io.crisppdf.CrispPDF.desktop` and `scripts/install.sh`.
- Local application state: `~/.config/crisppdf/state.json`.
- Installed user binary: `~/.local/bin/crisppdf`.
- Desktop application ID: `io.crisppdf.CrispPDF`.

Do not version `target/`, packaging output, caches, logs, renders, or private agent state.
