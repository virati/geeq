# geeq

A minimal, fast, geeqie-style image viewer written in Rust with [egui](https://github.com/emilk/egui)/[eframe](https://github.com/emilk/egui).

![geeq screenshot](docs/screenshot.png)

## Commands

Install, then add `cgeeq` and `pgeeq` as links to the same binary (once;
they keep working across reinstalls):

```sh
cargo install --path .
ln -sf geeq ~/.cargo/bin/cgeeq
ln -sf geeq ~/.cargo/bin/pgeeq
```

Run:

```sh
geeq [file-or-folder]    # compact UI with the Files panel open
cgeeq [file-or-folder]   # compact UI, just the picture
pgeeq [file-or-folder]   # photography: full UI with EXIF/histogram and folder plots
```

The compact UI is a full-window picture with everything else in the
right-click radial menu. Press and hold the right button, move toward an
item, and let go to pick it; a quick right-click leaves the menu open for
clicking. `C`, Options or Full in the radial menu switches to the full UI
(folder list, EXIF/histogram panel, rotate bar) and back.

Video mode: add `--video` (or `-V`) to either command.

The Files panel header has dropdowns next to the file count: 🎬 Video (hide / list / load previews / videos only), 📄 PDF (show / hide), 📷 Raw (show / hide), and ⇅ sort, which cycles name (A→Z) → time (newest first) → size (largest first) on each click; Sort in the radial menu does the same. Folders always list by name. The sort is saved in `~/.config/geeq/sort.txt`; PDF and Raw settings are saved in `~/.config/geeq/show_pdfs.txt` and `show_raws.txt`.

kgallery: press `K` (or start with `--kgallery`) to play the videos of the folder together in a looping NxN grid (needs libmpv). The videos are picked at random from the whole folder: the order is shuffled each time kgallery opens, and paging through the grid shows every video once. `Left`/`Right` change page (the next and previous pages are preloaded), `+`/`-` change the grid size (columns x rows, up to 8x6, not always square), `Space` pauses, `F11` is fullscreen, `Esc`/`Q` returns. `GEEQ_HWDEC` overrides mpv's hardware decoding (default `auto-safe`).

kgallery grid: `--nn 5` (or `--nn=5`) makes it a square 5x5 grid (1 to 6). Without it the grid is picked from the number of videos.

kgallery shuffle: `geeq --kgallery --shuffle` starts every video at a random spot (0 to 90 percent of its length). Paging back to a page, and the slideshow replay, pick new random spots. After the first pass each clip loops from its start.

kgallery sound output: always the HDMI out (`pulse/alsa_output.pci-0000_01_00.1.hdmi-stereo`); `GEEQ_AUDIO_DEVICE` overrides it.

kgallery sound: one tile has the sound, framed in yellow. By default it is the tile under the mouse; `R` cycles through the tiles every 5 seconds, `F` goes back to the tile under the mouse (`A` toggles between the two). `M` mutes and unmutes (the title shows "muted").

kgallery slideshow: `S` toggles it. Each page plays every video N times (`[` / `]`, default 2), waits T seconds after the last play (`,` / `.`, default 2.0, steps of 0.5) and moves to the next page, wrapping at the end of the folder. In a grid, a clip that finished its plays holds its last frame until the longest clip on the page is done.

To play the shown video, pick Player in the radial menu or the 🎞 smplayer button above the frame sheet (needs `smplayer` on `PATH`).

## Features

- **Folder navigator + file list** on the left, open at start in `geeq` and `pgeeq`, toggled with Files in the radial menu (`.`/`..` entries, hidden-file toggle, recently-active directories weighted by usage)
- **Opens on the top photo**: launching geeq or entering a folder opens the folder's first image (or the file you passed on the command line)
- **Thumbnail grid**: `G` (or Options → Thumbnail grid) swaps the image view for a grid of the whole folder. Arrow keys move between cells, `Enter` or a click opens the photo, and right-click adds it to the clip stack. Turn on Options → *Start in thumbnail grid* to open geeq in the grid every time (saved to `~/.config/geeq/start_in_grid.txt`)
- **Fast keyboard browsing**: `↑`/`↓` or `[`/`]` step through images, with fast auto-repeat while held
- **Image view** that fits the available space, with GPU-cheap zoom (scroll wheel or `+`/`-`, centered on the viewport) and drag-to-pan
- **Display-only rotation** via Rotate in the radial menu (90° per pick), or the left/right/reset bar in the full UI (doesn't touch the file on disk)
- **EXIF**: key fields (date, ISO, shutter speed, f-stop, focal length) shown up front, everything else in a collapsible section
- **RGB histogram** and a small radial HUD overlay of the current shot's exposure settings
- **Directory-wide analytics**: a 3-axis exposure phase plot (f-stop / ISO / shutter speed) and a sharpness (Laplacian variance) histogram across every image in the folder, both scanned in the background
- **Clip stack**: right-click a file (or press `\` on the open image) to add/remove it from a running stack; the stack is copied to the clipboard as shell-quoted paths for pasting into a terminal (`cp`/`mv`/etc.), with a thumbnail popup showing what's queued
- **Numbered tags**: press `1`–`9` on the open photo (or the selected grid cell) to add/remove it from that numbered tag, like the clip stack: each press puts the whole tag's list on the clipboard, shell-quoted. Tagged files show `[1,3]` in the file list, grid and caption. A tags popup (bottom right) has **Copy** (list back on the clipboard), **Pull RAW** and **Clear** per tag. While filter mode (`F`) is on, the number keys toggle looks instead. Tags live for the session only
- **Pull RAW**: for each photo in a tag, finds the raw with the same name on the camera card and copies it next to the photo (`.ARW`, `.RW2`, `.CR2`, … any raw extension; existing files are never overwritten). The card is found by name, not by path, so it does not matter that the JPEGs were copied elsewhere first: the mounted drives under `/run/media/$USER` and `/media` that have a `DCIM` folder are searched, the card folder named like the photo's folder first (`139_PANA`), then every other folder on the card. To use a card mounted elsewhere, put its mount point (or its `DCIM` folder) in `~/.config/geeq/card_root.txt`
- **Live filters**: load the looks exported from [snaplite](../snaplite) (Looks → Export looks → `snaplite-looks.json`) via Options → Live filters → Load looks. With filter mode on (`F`), toggle looks with the number keys `1`–`9` or the list; they stack in the order switched on and are applied on the fly to every image as you browse. Display-only, like rotation: files on disk are never touched. The pixel pipeline is a Rust port of snaplite's engine; geometry (crop/rotate) isn't part of a look.
- **Slideshow / screensaver**: `S` (or Options → Slideshow) goes fullscreen and cycles the folder every 3–30 s (default 7, optional shuffle) with the currently selected live filters applied. `Esc`, `S` or a click exits; arrow keys still step manually
- **Animated GIFs and WebPs** play in the viewer, showing the first frame while the rest decode (Pause in the radial menu, frame counter in the caption); live filters apply to every frame
- **Camera raw / DNG**: `.dng`, `.raw`, `.rw2`, `.cr2`/`.cr3`, `.nef`, `.arw`, `.raf`, `.orf`, `.pef` and more open like any other photo (via [rawler](https://crates.io/crates/rawler)). Uses the embedded full-size preview when the file has one, otherwise develops the raw data. Display only; the files are never touched
- **PDF preview**: `.pdf` files list like photos and open as pages 1 and 2 side by side (one page for a one-page PDF), rendered at screen size by poppler's `pdftoppm`, both pages at once. Grid thumbnails render at thumbnail size. Zooming in re-renders the pages sharp at the shown size once the zoom holds still. The Info panel shows the document info (title, pages, author, page size, dates) and the caption the page count. Reader (radial menu, or the full UI header) opens the PDF in your default PDF app
- JPEG decoding via libjpeg-turbo (`turbojpeg`); everything else via the `image` crate

## Running

```sh
cargo run --release
```

## Building

Requires a system install of `libjpeg-turbo` (for the `turbojpeg` crate). PDF preview needs poppler's `pdftoppm` on `PATH`.
