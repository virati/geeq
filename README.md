# geeq

A minimal, fast, geeqie-style image viewer written in Rust with [egui](https://github.com/emilk/egui)/[eframe](https://github.com/emilk/egui).

![geeq screenshot](docs/screenshot.png)

## Features

- **Folder navigator + file list** on the left (`.`/`..` entries, hidden-file toggle, recently-active directories weighted by usage)
- **Fast keyboard browsing**: `↑`/`↓` or `[`/`]` step through images, with fast auto-repeat while held
- **Image view** that fits the available space, with GPU-cheap zoom (scroll wheel or `+`/`-`, centered on the viewport) and drag-to-pan
- **Display-only rotation** via a bottom bar under the image (doesn't touch the file on disk)
- **EXIF**: key fields (date, ISO, shutter speed, f-stop, focal length) shown up front, everything else in a collapsible section
- **RGB histogram** and a small radial HUD overlay of the current shot's exposure settings
- **Directory-wide analytics**: a 3-axis exposure phase plot (f-stop / ISO / shutter speed) and a sharpness (Laplacian variance) histogram across every image in the folder, both scanned in the background
- **Clip stack**: right-click a file (or press `\` on the open image) to add/remove it from a running stack; the stack is copied to the clipboard as shell-quoted paths for pasting into a terminal (`cp`/`mv`/etc.), with a thumbnail popup showing what's queued
- **Live filters**: load the looks exported from [snaplite](../snaplite) (Looks → Export looks → `snaplite-looks.json`) via Options → Live filters → Load looks. With filter mode on (`F`), toggle looks with the number keys `1`–`9` or the list; they stack in the order switched on and are applied on the fly to every image as you browse. Display-only, like rotation: files on disk are never touched. The pixel pipeline is a Rust port of snaplite's engine; geometry (crop/rotate) isn't part of a look.
- **Slideshow / screensaver**: `S` (or Options → Slideshow) goes fullscreen and cycles the folder every 3–30 s (default 7, optional shuffle) with the currently selected live filters applied. `Esc`, `S` or a click exits; arrow keys still step manually
- **Animated GIFs** play in the viewer (pause/play and frame counter in the bottom bar); live filters apply to every frame
- JPEG decoding via libjpeg-turbo (`turbojpeg`); everything else via the `image` crate

## Running

```sh
cargo run --release
```

## Building

Requires a system install of `libjpeg-turbo` (for the `turbojpeg` crate).
