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
| `lyrics.rs` | Ambil lirik dari LRCLIB (`https://lrclib.net/api`) via WinRT HttpClient, dengan retry dan cache. |
| `battery.rs` | Status baterai, notifikasi power setting, event alert (colok/cabut, 20%/10%, penuh). |
| `volume.rs` | Volume/mute perangkat output default lewat COM callback (`#[implement]`), ikut perangkat default yang berganti. |
| `volume_keys.rs` | Hook keyboard low-level yang mengambil alih tombol volume agar pop-up volume Windows tidak muncul. |
| `outputs.rs` | Daftar perangkat output audio dan ganti default via `IPolicyConfig` (antarmuka tak resmi). |
| `bluetooth.rs` | Poll perangkat Bluetooth (classic + LE) tiap 2 s, lapor connect/disconnect. |
| `notifications.rs` | Poll toast di notification center (`UserNotificationListener`) tiap 0.7 s; klik membuka aplikasi pengirim. |
| `controls.rs` | Control Center: radio Wi-Fi/Bluetooth, airplane mode, brightness (WMI untuk layar internal, DDC/CI untuk monitor eksternal) di worker thread. |
| `tray.rs` | Ikon tray + menu (submenu pengaturan, Start at login, Exit); ditambah ulang saat Explorer restart. `on_click` mengembalikan `Settings` baru, `window.rs` yang menyimpan dan menerapkannya. |
| `settings.rs` | Pengaturan pengguna (alert per jenis, durasi alert, lebar notch, jeda Bluetooth) sebagai DWORD di `HKCU\Software\notch-woy`; nilai hilang/tidak valid kembali ke default. |
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

  Nomor baru harus unik; `WM_APP + 11` adalah yang berikutnya yang bebas.
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
