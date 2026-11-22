# sc-app

Hardpoint, the desktop app (the name is set in `src/brand.rs` as `APP_NAME` and `DISPLAY_NAME`; the exe is `hardpoint.exe`): the control service in-process, the dashboard in a native window, detached tabs on their displays, bezel buttons and a tray icon. Windows only for the windows; elsewhere it runs headless for testing. The app is API only: it presses no keys (the Stream Deck plugin keeps its own keybinds).

## Command line

See `hardpoint --help`. `--no-splash` skips the launch splash.

## App settings file

`app-settings.json` in the settings folder (the same folder as `dashboards.json`). Every key is optional; a missing file, a file that is not a JSON object or a key with the wrong type falls back to the default and is logged. Unknown keys are ignored. The Settings window will edit this file once it is built.

| Key | Type | Default | Effect |
|---|---|---|---|
| `splash` | boolean | `true` | `false`: no splash at launch |

```json
{ "splash": false }
```

## Splash

At launch on Windows the app shows a borderless 480 x 300 window, centred on the main display, with the Orbit Reticle icon and the working name. It opens while the service, the dashboard window and the tray start, and does not hold them up. It closes 2 s after it opened or when the dashboard page has loaded, whichever is later, and after 6 s at most; the dashboard window is then shown. Not shown with `--no-splash`, with `"splash": false`, or in headless runs.

## Artwork

`assets/orbit-reticle.svg` (colour) and `assets/orbit-reticle-mono.svg` (white, for the tray) are the Orbit Reticle icon. The PNGs beside them are those SVGs rasterised with Chromium: `icon-32.png` (title bar), `icon-256.png` (taskbar and Alt+Tab) and `tray-32.png` (tray). `hardpoint.ico` holds the colour icon at 16, 24, 32, 48, 64 and 256 px (the PNGs, rasterised the same way) and is the exe's Explorer icon, embedded by `build.rs` with the version information. Re-render them when the SVGs change. `assets/splash.html` is the splash page; the icon and the name are put in at run time.
