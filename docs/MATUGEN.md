# Matugen integration

Matugen support is **opt-in**. Limusic never reads or watches a Matugen file unless the
"Matugen" toggle under Settings → Themes is enabled.

## What it does

While enabled, Limusic injects the generated CSS from your Matugen output file as a
stylesheet and adds the `matugen-theme` class to `<html>`. The generated palette then owns
the application's color tokens, including the accent. Wallpaper regeneration by Matugen
updates `matugen.css`; Limusic reloads it live.

## Expected CSS path

Limusic reads its generated CSS from the application config directory:

- Linux: `~/.config/limusic/matugen.css`
- (derived from `BaseDirectory::Config` + `limusic/matugen.css` in `src-tauri/src/matugen.rs`)

Do not point Matugen anywhere else.

## Repository template

The committed example template is:

- `templates/limusic.css`

## Configure Matugen

1. Copy `templates/limusic.css` into your Matugen templates directory.
2. Add a template entry to your Matugen config, for example:

```toml
[templates.limusic]
input_path = "~/.config/matugen/templates/limusic.css"
output_path = "~/.config/limusic/matugen.css"
```

Adjust the paths to match your setup; the `output_path` must be Limusic's expected path
above.

3. Run Matugen against an image:

```sh
matugen image /path/to/wallpaper.png
```

Or otherwise regenerate after changing wallpaper.

## Enable in Limusic

Open Settings → Themes and toggle **Matugen** on. Limusic starts its file watcher and loads
the generated palette. With the toggle off (the default), no watcher starts and the
`limusic/matugen.css` file is not read or created.

## Disable

Toggle **Matugen** off. Limusic stops the watcher, unregisters its listener, removes the
injected stylesheet and restores the normal theme. Your template and generated CSS file are
not deleted.

## Troubleshooting

- **No colors change:** verify Matugen wrote `limusic/matugen.css` at the expected path and
  that the template is configured with that `output_path`.
- **Invalid template syntax:** validate by running `matugen image <file>`; check that the
  generated file contains `html.matugen-theme` and `html.matugen-theme.dark` selectors.
- **Watcher not updating:** make sure Matugen writes (not just prints) the file, then toggle
  Matugen off and on again.
