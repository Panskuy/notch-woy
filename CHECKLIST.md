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
