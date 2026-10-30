# AGENT.md — konteks proyek notch-woy

**notch-woy**: "Dynamic Island" untuk Windows 10/11 (x64), ditulis dalam Rust.
Notch hitam kecil di atas layar menampilkan lagu yang diputar dan baterai,
lalu membuka player, lirik tersinkron, dan Control Center saat di-hover.
Satu exe, tanpa framework GUI: semua digambar sendiri dengan `tiny-skia` lalu
ditampilkan lewat layered window Win32.

- Repo: https://github.com/Panskuy/notch-woy (lisensi MIT)
- Versi di `Cargo.toml`: 0.2.0 (README dan rilis terakhir masih menyebut v0.1.0)
- Checklist kerja dan rilis: [CHECKLIST.md](CHECKLIST.md)
- Rust edition 2024, target MSVC; hanya Windows

## Struktur folder

```
Cargo.toml        dependensi; fitur crate `windows` didaftarkan satu per satu
build.rs          embed ikon + version info ke exe (winresource, butuh rc.exe)
assets/
  fonts/          Inter (Regular/SemiBold/Bold/ExtraBold) + OFL.txt
  notch-woy.ico   dibuat oleh test `make_icon` di src/icon.rs
docs/             screenshot untuk README (dirender dari data contoh)
src/              semua kode (lihat tabel di bawah)
target/, dist/    output build, di-.gitignore
```

## Modul `src/`

| File | Peran |
|---|---|
| `main.rs` | Entry point: `controls::init_com_security()`, autostart default (hanya release), lalu `window::run()`. Release build tanpa console. |
| `window.rs` (~1800 baris) | Inti aplikasi. Struct `App` (state global di `thread_local! APP`), `wndproc`, timer, hover/klik/scroll/drag, alert, deteksi jendela maximized/fullscreen, `Surface` (DIB BGRA untuk `UpdateLayeredWindow`), jendela shadow terpisah. |
| `render.rs` (~2500 baris) | Semua gambar dan layout. Konstanta `Shape` (COLLAPSED, EXPANDED, MINI, *_ALERT), `Scene` yang diisi window.rs, `draw()`, `draw_shadow()`, hit-test (`button_at`, `control_at`, `progress_at`, `lyric_at`, `output_at`), `Fonts` + glyph cache, helper teks (`wrap`, `ellipsize`). |
| `anim.rs` | `Spring` (pegas teredam, langkah integrasi tetap) dan fungsi easing. |
| `font.rs` | Rasterisasi teks di atas `ab_glyph` (glyph di-decode saat dipakai, hemat memori). |
| `palette.rs` | Ambil warna tint dari cover album. |
| `media.rs` | Sesi "now playing" Windows (GlobalSystemMediaTransportControls), poll 500 ms, decode cover, kirim `Command` (play/pause/next/seek). |
| `lyrics.rs` | Ambil lirik dari LRCLIB (`https://lrclib.net/api`) via WinRT HttpClient, dengan retry dan cache. Parser LRC termasuk waktu per kata (enhanced LRC `<mm:ss.xx>`, `Line::words`); `Lyrics::sung` = berapa karakter baris sudah dinyanyikan (dari waktu kata, atau diperkirakan dari jarak ke baris berikutnya). Saat lirik tersinkron tampil dan diputar, `PROGRESS_TIMER` jalan 33 ms (`LYRICS_INTERVAL_MS`), selain itu 100 ms. |
| `battery.rs` | Status baterai, notifikasi power setting, event alert (colok/cabut, 20%/10%, penuh). |
| `volume.rs` | Volume/mute perangkat output default lewat COM callback (`#[implement]`), ikut perangkat default yang berganti. |
| `meter.rs` | Peak meter WASAPI (`IAudioMeterInformation`) perangkat output default, dibaca di thread UI hanya saat equalizer tampil (`App::equalizer_levels`); `Bars` mengubah puncak kiri/kanan jadi tinggi bar (dB, dikurangi rata-rata berjalan, tunda per bar). Hening > 2 s saat lagu diputar → `None`, equalizer kembali animasi sendiri. |
| `volume_keys.rs` | Hook keyboard low-level yang mengambil alih tombol volume agar pop-up volume Windows tidak muncul. |
| `outputs.rs` | Daftar perangkat output audio dan ganti default via `IPolicyConfig` (antarmuka tak resmi). |
| `bluetooth.rs` | Poll perangkat Bluetooth (classic + LE) tiap 2 s, lapor connect/disconnect (jeda 8 s + jeda ulang yang bisa diatur). Baterai: properti PnP `{104EA319-…} 2` lewat CfgMgr32 (`read_batteries`), dicocokkan per alamat Bluetooth. |
| `notifications.rs` | Poll toast di notification center (`UserNotificationListener`) tiap 0.7 s; klik membuka aplikasi pengirim. |
| `controls.rs` | Control Center: radio Wi-Fi/Bluetooth, airplane mode, brightness (WMI untuk layar internal, DDC/CI untuk monitor eksternal) di worker thread. |
| `tray.rs` | Ikon tray + menu (Settings…, Start at login, Exit); ditambah ulang saat Explorer restart. |
| `settings.rs` | Pengaturan pengguna (alert per jenis, durasi alert, lebar notch Narrow/Normal/Wide/Wider, ukuran notch terbuka Compact/Normal/Large `OpenSize`, jeda Bluetooth) sebagai DWORD di `HKCU\Software\notch-woy`; nilai hilang/tidak valid kembali ke default. Nilai enum yang disimpan = discriminant-nya (jangan diubah; tambah varian baru di nomor berikutnya). `to_bits`/`from_bits` untuk mengirim lewat `wparam`. |
| `plugins.rs` | Plugin gaya xbar/SwiftBar: file `{nama}.{interval}.{ext}` (`.ps1`, `.cmd`/`.bat`, `.py`, `.exe`) di `%LOCALAPPDATA%\notch-woy\plugins`, dijalankan sebagai proses tanpa jendela di thread worker (batas 10 s, interval min 5 s) hanya bila `Settings::plugins` (default mati). Baris pertama sebelum `---` = judul (`\| href=`), baris pertama sesudahnya = isi; judul berubah → `WM_PLUGIN` → alert `Toast` dengan `link`. |
| `shelf.rs` | File shelf: daftar path (bukan salinan) di `%LOCALAPPDATA%\notch-woy\shelf.txt`, worker thumbnail (`IShellItemImageFactory`), drop target OLE (`IDropTarget`, hanya Copy/Link, tidak pernah Move), seret keluar (`SHDoDragDrop`, harus dipanggil tanpa borrow `App`), share (`DataTransferManager` lewat jendela jangkar 1x1 tak terlihat: share sheet menolak jendela `NOACTIVATE` seperti notch). |
| `settings_ui.rs` | Tampilan jendela pengaturan ala macOS System Settings, digambar sendiri dengan tiny-skia: layout (logis 700×420), gambar (`draw`), hit-test (`hit`, `control_rect`), urutan fokus. Fungsi murni; pratinjau PNG lewat test `settings_preview`. Memakai `render::Fonts` dan `render::draw` (pratinjau notch). |
| `settings_window.rs` | Host Win32 untuk `settings_ui`: jendela, `WM_PAINT` via `SetDIBitsToDevice`, mouse/keyboard, animasi `Spring` dengan timer 16 ms, tema terang/gelap + title bar DWM, DPI. Perubahan langsung disimpan dan dikirim ke notch via `WM_SETTINGS_CHANGED`. Langkah yang punya modal loop sendiri (menu pop-up, konfirmasi reset) dan penutupan jendela dijalankan setelah borrow state dilepas (`Modal`). |
| `autostart.rs` | Registry `HKCU\...\Run` dan `HKCU\Software\notch-woy\AutostartChosen`. |
| `icon.rs` | Hanya `#[cfg(test)]`: menggambar dan menulis `assets/notch-woy.ico`. |
| `memory.rs` | Hanya `#[cfg(test)]`: mengukur pemakaian memori per langkah. |

## Arsitektur

- **Satu thread UI** memiliki `App`; diakses lewat `with_app(|app| ...)`.
- **Worker thread** per sumber data (media, lyrics, bluetooth, notifications,
  controls, outputs, volume). Masing-masing menyimpan state di
  `Arc<Mutex<..>>` lalu memberi tahu jendela dengan `PostMessageW`:

  | Pesan | Nilai | Dari |
  |---|---|---|
  | `WM_MEDIA` | `WM_APP + 2` | media.rs |
  | `WM_LYRICS` | `WM_APP + 3` | lyrics.rs |
  | `WM_VOLUME` | `WM_APP + 4` | volume.rs (`wparam` = %, `lparam` = muted) |
  | `WM_TOAST` | `WM_APP + 5` | notifications.rs |
  | `WM_BLUETOOTH` | `WM_APP + 6` | bluetooth.rs |
  | `WM_CONTROLS` | `WM_APP + 7` | controls.rs |
  | `WM_TRAY` | `WM_APP + 8` | tray.rs |
  | `WM_VOLUME_KEY` | `WM_APP + 9` | volume_keys.rs |
  | `WM_OUTPUTS` | `WM_APP + 10` | outputs.rs |
  | `WM_SETTINGS_CHANGED` | `WM_APP + 11` | settings_window.rs (`wparam` = `Settings::to_bits`) |
  | `WM_SHELF` | `WM_APP + 12` | shelf.rs (ikon file siap) |
  | `WM_PLUGIN` | `WM_APP + 13` | plugins.rs (judul plugin berubah) |

  Nomor baru harus unik; `WM_APP + 14` adalah yang berikutnya yang bebas.
- **Halaman notch terbuka**: berdampingan kiri-kanan, `App::page` -100
  (slot kiri), 0 (tengah), 100 (kanan). Halaman mana di slot mana:
  `Settings::page_order` (default shelf, player, Control Center; diatur di
  Settings → Layout), halaman pembuka `Settings::start_page`. Jangan
  menganggap shelf = -100 dsb.: pakai `App::slot(Page)` / `render::slot_of`;
  lebar per slot `render::open_width(order, ...)`. Digeser sejauh lebar notch
  (`Canvas::dx`). `WM_MOUSEHWHEEL` (positif = kanan) dan `WM_MOUSEWHEEL`
  (turun = kanan; di shelf menggulir file dulu) lewat `turn_pages`;
  `rubber_band` dan `page_after_drag` untuk touchpad. Titik halaman di bawah.
  Gradien cover (saat lirik terbuka) adalah latar seluruh notch, tidak ikut
  bergeser; di halaman samping diredupkan (`SIDE_PAGE_TINT`).
  Lebar notch terbuka per halaman: `render::open_width` (shelf
  `shelf_width(count)` 400–520, player `panel_width`, Control Center
  `CONTROLS_WIDTH` 560); `App::retarget` dipanggil saat halaman atau jumlah
  file shelf berubah. Kanvas jendela 660 logis (`WIN_W`) membatasi lebar
  maksimum: Control Center 560 x Large 1,1 + bahu + pantulan pegas.
- **Open size** (`Settings::open_size`, `OpenSize::zoom` 0,9/1/1,1): notch
  terbuka diskalakan utuh. Bentuk target dikali zoom (`App::retarget`),
  isi ditata dalam satuan desain dan digambar dengan `scale * zoom`
  (`Scene::open_zoom`); semua hit-test notch terbuka memakai
  `App::open_scale()`, pengecekan "sudah terbuka" memakai `App::is_open()`.
  Klik di halaman shelf tidak diteruskan ke hit-test player.
- **Pacing animasi**: message loop di `window::run` berjalan per frame
  (`DwmFlush`, ikut vsync) selama notch *atau* jendela pengaturan
  beranimasi (`settings_window::is_animating` / `frame`). Animasi yang bisa
  berjalan lama (equalizer notch diam, lirik per kata) juga lewat loop ini
  (`App::ambient`), tapi hanya tiap `ambient_every` refresh (~60 fps:
  `round(Hz / 60)` dari `DwmGetCompositionTimingInfo`) dan hanya `redraw`.
  Jangan pakai `SetTimer` untuk animasi: timer 16 ms terukur jalan tiap
  8–38 ms. Jangan memanggil hal yang lambat (RPC layanan audio, WMI, I/O)
  di jalur frame: anggaran di 180 Hz hanya 5,5 ms.
- **Memori**: `window::trim_working_set` (`EmptyWorkingSet`) dipanggil oleh
  `TRIM_TIMER` (id 8) saat notch diam dan saat jendela pengaturan ditutup.
  Timer id di `window.rs` yang terpakai: 1–4, 6–9 (5 bebas; `CLOCK_TIMER` 9: jam notch
  diam, dipasang ulang tiap pergantian menit; juga `WM_TIMECHANGE` dan resume).
- **Jendela biasa** (bukan notch) ditampilkan dengan `SetWindowPos(SWP_SHOWWINDOW)`,
  bukan `ShowWindow`: panggilan `ShowWindow` pertama di proses memakai show
  state dari peluncur (mis. tersembunyi), sehingga jendela tidak muncul.
- **Render**: `App::redraw` membangun `Scene`, `render::draw` menggambar ke
  `Pixmap` (RGBA premultiplied), `Surface::present` mengonversi ke BGRA dan
  memanggil `UpdateLayeredWindow`. Piksel transparan penuh meneruskan klik
  ke jendela di bawahnya.
- **Animasi**: bentuk notch (width/height/radius), halaman, lirik, toggle, dll.
  semuanya `Spring`. Animasi berjalan selama `App::is_animating()` true.
- **Satuan**: layout dalam piksel logis (96 DPI), dikali `scale` untuk piksel
  fisik. Proses per-monitor DPI aware v2. Kanvas jendela 600×200 logis.
- **COM**: `controls::init_com_security()` wajib dipanggil di main thread
  sebelum pemakaian COM lain (keperluan WMI). Worker thread memanggil
  `CoInitializeEx(COINIT_MULTITHREADED)` sendiri.

## Build, run, test

```
cargo build --release        # target/release/notch-woy.exe
cargo run                    # debug build dengan console untuk log
cargo test                   # unit test
cargo test <nama> -- --ignored   # test yang render PNG ke target/*.png atau menyentuh sistem nyata
```

Test `#[ignore]` yang berguna:
- `preview` (render.rs): render pratinjau ke `target/*.png`, dipakai untuk screenshot docs.
- `make_icon` (icon.rs): menulis ulang `assets/notch-woy.ico`.
- `memory_breakdown` (memory.rs): `cargo test --release memory_breakdown -- --ignored --nocapture --test-threads 1`.

Butuh toolchain Rust MSVC + Windows SDK (`rc.exe`). Profil dev memakai
`opt-level = 1` (dependensi `3`) karena render tanpa optimasi patah-patah.

## Konvensi kode

- Komentar `//!` di awal tiap modul menjelaskan perannya; komentar menjelaskan
  *kenapa* (keterbatasan API Windows, alasan memilih polling, dsb.), bukan apa.
- Konstanta layout/timing sebagai `const` di atas file, bukan angka langsung.
- Fitur crate `windows` yang baru dipakai harus ditambahkan ke daftar
  `features` di `Cargo.toml`.
- Debug build tidak pernah menyalakan autostart sendiri; jaga perilaku ini.
- Privasi: hanya judul/artis/album/durasi lagu yang dikirim keluar (ke LRCLIB);
  isi notifikasi tidak boleh meninggalkan mesin.
- Bila menambah fitur yang terlihat pengguna, perbarui README (tabel fitur,
  "Using it", "Good to know") dan screenshot di `docs/` bila perlu.
