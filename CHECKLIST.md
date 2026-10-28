# CHECKLIST.md — notch-woy

Checklist kerja untuk proyek ini. Konteks arsitektur ada di [AGENT.md](AGENT.md).

## Pekerjaan yang masih terbuka (per 2026-10-29)

- [ ] README masih menyebut `v0.1.0` (nama zip di bagian Install, judul
      commit screenshot), padahal `Cargo.toml` dan tag git sudah `v0.2.0`.
- [ ] `dist/` sudah berisi `notch-woy-v0.2.0-windows-x64.zip` tetapi belum ada
      `dist/release-notes-v0.2.0.md`.
- [ ] Pemilih perangkat output audio (`src/outputs.rs`, tombol di player)
      belum disebut di README: tabel fitur maupun "Using it".
- [ ] Belum ada screenshot pemilih output di `docs/`.
- [ ] Arah scroll ke Control Center tidak konsisten: README bilang "scroll
      up", release notes v0.1.0 bilang "scroll down". Cek perilaku di
      `App::on_wheel` (`src/window.rs`) lalu samakan dokumentasinya.

- [ ] Verifikasi perbaikan 2026-10-29 di mesin nyata: (1) restart laptop,
      buka aplikasi fullscreen/maximized, notch harus langsung terlihat;
      (2) keyboard Bluetooth yang tidur lalu tersambung lagi tidak memicu
      alert berulang. Belum di-commit.

## Ide fitur baru (backlog)

Disusun dari aplikasi sejenis:
[boring.notch](https://github.com/TheBoredTeam/boring.notch) (macOS, open source),
[NotchNook](https://www.macworld.com/article/2406934/notfhnook-macbook-dynamic-island-widgets-files-tray.html),
[NotchDrop](https://github.com/Lakr233/NotchDrop),
[DynamicWin](https://github.com/FlorianButz/DynamicWin) (Windows),
Notchify dan Venu (Windows, closed source), serta aplikasi timer seperti
NotchFlow dan NotchNest. Urutan = perkiraan manfaat dibanding usaha.
Sebelum mulai satu item, cek dulu apakah API-nya jalan di aplikasi
**unpackaged** (tanpa MSIX); beberapa API WinRT butuh package identity.

### Prioritas tinggi

- [ ] **Pengaturan** (semua referensi punya; notch-woy hanya punya menu tray).
      Minimal: alert mana yang aktif (baterai, volume, Bluetooth, notifikasi,
      lagu baru), durasi tampil, lebar notch, jeda Bluetooth
      (`DISCONNECT_GRACE`/`REPEAT_QUIET`). Simpan di `HKCU\Software\notch-woy`
      (sudah dipakai `autostart.rs`) atau file TOML di `%APPDATA%`.
      Langkah awal termurah: centang per alert di menu tray (`tray.rs`).
- [ ] **Mode jangan ganggu / jeda alert** dari menu tray, dan idealnya ikut
      status Do Not Disturb Windows. Alert tetap tercatat untuk indikator.
- [ ] **Baterai perangkat Bluetooth** (DynamicWin): tampilkan persen baterai
      earbuds/keyboard di alert connect dan modul Bluetooth Control Center.
      Baca properti perangkat `{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2`
      lewat `DeviceInformation` di `bluetooth.rs`.
- [ ] **Indikator kamera/mikrofon sedang dipakai** (titik hijau/oranye seperti
      iOS). Windows mencatatnya di
      `HKCU\...\CapabilityAccessManager\ConsentStore\{webcam,microphone}`:
      `LastUsedTimeStop == 0` berarti sedang dipakai. Poll seperti
      `notifications.rs`, tampilkan titik di notch istirahat.
- [ ] **Alert kecerahan (brightness HUD)** seperti alert volume (boring.notch:
      "system HUD replacement"). Event WMI `WmiMonitorBrightnessEvent`;
      WMI sudah dipakai di `controls.rs`.
- [ ] **Multi-monitor**: pilih monitor notch, atau ikut monitor jendela aktif.
      Sekarang `relayout` selalu memakai monitor utama.

### Prioritas sedang

- [ ] **File shelf / tray** (boring.notch, NotchNook, NotchDrop, DynamicWin):
      seret file ke notch untuk disimpan sementara, seret keluar untuk
      memindah, tombol share (dialog Share Windows, `DataTransferManager`).
      Butuh `IDropTarget` (OLE drag & drop) di jendela notch; perhatikan
      piksel transparan penuh meneruskan klik, jadi area drop harus terlihat.
- [ ] **Visualizer dari audio asli**: equalizer lagu baru sekarang animasi.
      `IAudioMeterInformation` (peak meter WASAPI) di perangkat output default
      cukup murah; `volume.rs` sudah memegang endpoint-nya.
- [ ] **Timer / stopwatch / Pomodoro** sebagai live activity (NotchFlow,
      NotchNest): sisa waktu di notch istirahat, alert saat selesai.
- [ ] **Mixer volume per aplikasi** di Control Center (seperti EarTrumpet),
      melengkapi pemilih output yang sudah ada (`IAudioSessionManager2`).
- [ ] **Alert Caps Lock / Num Lock dan ganti bahasa input** (hook keyboard
      sudah ada di `volume_keys.rs`).
- [ ] **Sisa waktu baterai** (sampai penuh/habis) di modul baterai Control
      Center: `SYSTEM_POWER_STATUS.BatteryLifeTime` sudah dibaca `battery.rs`.
- [ ] **Shortcut keyboard global** untuk membuka notch (`RegisterHotKey`),
      sekaligus jalan untuk aksesibilitas keyboard.
- [ ] **Cache lirik ke disk** agar lagu yang sama tidak meminta LRCLIB lagi
      dan lirik tetap ada saat offline.

### Prioritas rendah / perlu riset

- [ ] **Kalender / acara berikutnya** (boring.notch, NotchNook, DynamicWin).
      `AppointmentManager` kemungkinan butuh package identity; alternatif
      URL ICS (Google/Outlook). Perlu dicatat di bagian Privasi README.
- [ ] **Cek update dari GitHub Releases** (DynamicWin punya auto updater):
      cukup beri tahu ada versi baru di menu tray. Ini koneksi keluar baru,
      jadi wajib opsional dan dicatat di README.
- [ ] **Cuaca** (roadmap boring.notch): butuh lokasi dan layanan luar; hanya
      jika opsional.
- [ ] **Gesture yang bisa diatur** (roadmap boring.notch), mis. swipe
      horizontal untuk next/previous lagu.
- [ ] **Ekstensi / plugin** (roadmap boring.notch): baru masuk akal setelah
      pengaturan ada.

### Tidak cocok untuk Windows

- Widget lock screen (Alcove, boring.notch): lock screen Windows berjalan di
  desktop aman, aplikasi biasa tidak bisa menggambar di sana.
- AirDrop: tidak ada padanannya; gantinya dialog Share Windows / Nearby Share.
- Cermin kamera (boring.notch): bisa dibuat, tapi butuh izin kamera dan
  menambah ukuran exe; manfaatnya kecil di laptop Windows.

## Sebelum mengubah kode

- [ ] Baca [AGENT.md](AGENT.md) dan komentar `//!` di modul yang disentuh.
- [ ] Pastikan perubahan tetap di thread yang benar: state UI hanya di
      `App` (thread UI), panggilan sistem yang lambat di worker thread.

## Setelah mengubah kode

- [ ] `cargo build` tanpa warning baru.
- [ ] `cargo test` lulus.
- [ ] Bila tampilan berubah: `cargo test preview -- --ignored` lalu cek PNG di `target/`.
- [ ] `cargo run` dan coba manual: hover, buka/tutup, scroll antar halaman,
      klik tombol, drag progress bar dan slider.
- [ ] Cek di DPI lain (100% dan 125%/150%) bila layout atau ukuran berubah.
- [ ] Bila memakai API Windows baru: fiturnya sudah ditambahkan ke
      `features` crate `windows` di `Cargo.toml`.
- [ ] Debug build tetap tidak menyalakan autostart sendiri.

## Menambah sumber data / worker baru

- [ ] Modul baru di `src/` dengan komentar `//!` yang menjelaskan perannya
      dan alasan pilihan desain (mis. kenapa polling).
- [ ] Daftarkan `mod` di `src/main.rs`.
- [ ] Worker memanggil `CoInitializeEx(COINIT_MULTITHREADED)` bila memakai COM/WinRT.
- [ ] State bersama lewat `Arc<Mutex<..>>`, beri tahu jendela dengan
      `PostMessageW` memakai konstanta `WM_APP + n` yang unik
      (berikutnya yang bebas: `WM_APP + 11`). Perbarui tabel pesan di AGENT.md.
- [ ] Tangani pesannya di `wndproc` (`src/window.rs`) dan simpan state di `App`.
- [ ] Gambar di `render.rs` lewat `Scene`; hit-test ditaruh di `render.rs`
      bersama layout-nya.
- [ ] Bila ada alert baru: varian `Alert`, `Shape` di `alert_shape`, dan durasi
      tampil (`*_ALERT_MS`) di `window.rs`.

## Privasi

- [ ] Tidak ada data baru yang dikirim keluar mesin. Bila ada, tulis di
      bagian "Good to know → Privacy" README.
- [ ] Isi notifikasi tetap hanya ditampilkan secara lokal.

## Rilis

- [ ] Naikkan `version` di `Cargo.toml` (dan `Cargo.lock` lewat `cargo build`).
- [ ] `cargo test` dan `cargo build --release` bersih.
- [ ] Uji `target/release/notch-woy.exe` di luar `cargo`: autostart menyala
      pada run pertama, menu tray (Start at login, Exit) berfungsi.
- [ ] Perbarui screenshot di `docs/` bila tampilan berubah
      (`cargo test preview -- --ignored`).
- [ ] Perbarui README: nama zip di Install, fitur baru, "Using it", "Good to know".
- [ ] Buat `dist/notch-woy-vX.Y.Z-windows-x64.zip` berisi `notch-woy.exe`.
- [ ] Tulis `dist/release-notes-vX.Y.Z.md` (format seperti
      `release-notes-v0.1.0.md`), termasuk SHA-256 zip:
      `Get-FileHash dist\notch-woy-vX.Y.Z-windows-x64.zip`.
- [ ] Commit, tag `vX.Y.Z`, push tag, lalu buat GitHub Release dengan zip dan
      release notes.
