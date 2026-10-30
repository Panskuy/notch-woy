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
- [x] Arah scroll ke Control Center tidak konsisten (README "scroll up",
      release notes v0.1.0 "scroll down"). Selesai dengan halaman kiri-kanan
      (lihat "Halaman kiri-kanan" di bawah); README sudah disamakan. Release
      notes v0.1.0 dibiarkan (dokumen rilis lama).

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

- [x] **Pengaturan** lewat menu tray (2026-10-30, `settings.rs`): alert per
      jenis, durasi alert (Short/Normal/Long), lebar notch
      (Normal/Wide/Wider), jeda Bluetooth (0/1/5/15/60 menit). Disimpan di
      `HKCU\Software\notch-woy`. Mematikan alert volume mengembalikan tombol
      volume ke Windows. Jeda putus Bluetooth (`DISCONNECT_GRACE`, 8 s)
      sengaja tetap: itu penyaring, bukan selera.
  - [x] **Jendela pengaturan sendiri** (2026-10-30, `settings_window.rs`),
        dibuka dari tray → *Settings…*; submenu pengaturan di tray dihapus
        agar tidak ada dua tempat. Perubahan langsung berlaku (tanpa OK),
        ada *Reset to defaults*, Tab/Esc berfungsi, ikut DPI per monitor.
        Opsi yang dibandingkan:
        | Opsi | Dependensi | Ukuran/memori | Aksesibilitas | Keputusan |
        |---|---|---|---|---|
        | Kontrol Win32 native + manifest comctl v6 | tidak ada | ±0 | bawaan Windows | **dipilih** |
        | Digambar sendiri (tiny-skia) | tidak ada | ±0 | harus dibuat sendiri | terlalu banyak kode |
        | egui/eframe | banyak (GPU/GL) | exe & memori naik | terbatas | terlalu berat |
        | native-windows-gui / winsafe | 1 crate | kecil | sama | hanya pembungkus API yang sudah ada |
        Referensi: pengaturan boring.notch (`SettingsView.swift`, tab + bagian
        "Notch sizing"); Microsoft Learn "Enabling Visual Styles" dan
        Raymond Chen soal activation context comctl v6.
  - [x] **Lebar Narrow** (−40 px). Tata ulang isi notch ternyata tidak perlu:
        pratinjau kasus terpadat (cover, equalizer, Bluetooth, baterai dicas)
        dan tanpa baterai tetap rapi (`preview_collapsed_narrow*.png`).
        Disimpan sebagai nilai 3 agar pengaturan lama tetap terbaca.
  - [x] **Tampilan ala Apple** (2026-10-30, `settings_ui.rs`): keputusan
        kontrol native di atas dibalik atas permintaan; kontrol Win32 tidak
        bisa dibuat bergaya Apple. Sekarang digambar sendiri dengan tiny-skia
        dan font Inter yang sudah ada (exe hanya +62 KB, tanpa dependensi
        baru): sidebar inset ala macOS System Settings (General, Alerts,
        Notch), kartu baris bersudut, ikon tile berwarna, switch hijau
        beranimasi, segmented control dengan sorotan bergeser, tombol pop-up
        dengan chevron, pratinjau notch asli yang lebarnya beranimasi.
        Terang/gelap mengikuti Windows (`AppsUseLightTheme`), title bar ikut
        (`DWMWA_USE_IMMERSIVE_DARK_MODE`, `DWMWA_CAPTION_COLOR` di Win 11).
        Keyboard: Tab/Shift+Tab, panah (sidebar, segmen), Space/Enter, Esc;
        cincin fokus biru setelah dipakai keyboard. Warna = system colors
        Apple (HIG). Pratinjau: `cargo test settings_preview -- --ignored`.
  - [x] **Animasi pengaturan mulus** (2026-10-30). Diukur: timer 16 ms
        ternyata jalan rata-rata tiap 24,8 ms dengan jeda 8–38 ms (~40 fps,
        patah-patah; layar laptop 180 Hz). Sekarang frame dipacu `DwmFlush`
        lewat message loop utama seperti notch (5,0–6,3 ms, ikut vsync).
        Render 1–1,4 ms per frame; pratinjau notch di-cache dan mask
        selebar jendela per frame dihapus. Alat ukur: `cargo test --release
        times_frames|timer_intervals|dwm_flush_intervals -- --ignored --nocapture`.
  - [ ] Keterbatasan UI ala Apple: (1) screen reader tidak bisa membaca
        isinya (tidak ada kontrol native; perlu UI Automation provider bila
        dibutuhkan); (2) menu pop-up Bluetooth adalah menu Win32, tetap terang
        di tema gelap (menu gelap hanya lewat API uxtheme tak terdokumentasi);
        (3) pindah ke monitor dengan skala lain belum diuji nyata.
  - [ ] Bila perlu: lebar lebih sempit dari Narrow. Perkiraan dari pratinjau:
        di Narrow (190 px dengan baterai) jarak equalizer ke ikon Bluetooth
        ~58 px, jadi di bawah ~140 px keduanya bertabrakan; isi notch
        istirahat harus ditata ulang dulu (mis. sembunyikan equalizer).
- [x] **RAM serendah mungkin** (2026-10-30). Diukur per subsistem
      (`cargo test --release memory_breakdown -- --ignored --nocapture
      --test-threads 1`): Bluetooth 3,3 MB (WinRT device enumeration, biaya
      sekali, bukan bocor: `bluetooth_over_time`), media 1,4 MB, notifikasi
      0,9 MB, sisanya kecil. Tindakan:
      - `EmptyWorkingSet` saat diam: 30 s setelah start, 30 s setelah notch
        kembali istirahat, lalu tiap 5 menit selama diam, dan saat jendela
        pengaturan ditutup. "Memory" di Task Manager: ~9–11,5 MB → ~3–4,5 MB.
        Catatan jujur: ini mengembalikan halaman yang tak disentuh ke
        Windows (standby list, bisa dipakai aplikasi lain); commit/private
        bytes (~11–12 MB, mayoritas komponen WinRT Windows) tidak berubah.
      - Jendela pengaturan tanpa buffer BGRA kedua (swap in place), semua
        dilepas saat ditutup.
  - [ ] Pengurangan commit yang sesungguhnya, bila perlu: (1) kanvas notch
        600×200 + DIB-nya (~1 MB di 125%) bisa dikecilkan saat istirahat dan
        dibesarkan saat hover; (2) ganti enumerasi Bluetooth WinRT (3,3 MB)
        dengan API yang lebih ringan, tapi uji ulang bug alert berulang.
- [ ] **Mode jangan ganggu / jeda alert** dari menu tray, dan idealnya ikut
      status Do Not Disturb Windows. Alert tetap tercatat untuk indikator.
- [x] **Baterai perangkat Bluetooth** (2026-10-30, `bluetooth.rs`). Alert
      connect menampilkan glyph baterai ala iOS (kuning/merah saat rendah)
      menggantikan "Connected"; modul Bluetooth Control Center menulis
      "Connected · 46%". Tanpa data baterai tetap "Connected".
      Riset & perbandingan:
      | Cara | Classic | LE | Biaya | Risiko |
      |---|---|---|---|---|
      | **Properti PnP `{104EA319-…} 2` via CfgMgr32** | ya | ya | 2,2 ms per baca, tanpa koneksi ke perangkat, RAM tak bertambah | rendah |
      | GATT Battery Service 0x2A19 (cara BlueGauge untuk LE) | tidak | ya | koneksi GATT per perangkat, async, thread notifikasi | *Access Denied*/perlu pairing |
      | WinRT `DeviceInformation` + AQS | ya | ya | DevQuery | sintaks AQS key kustom tak terdokumentasi |
      Dipilih PnP: diuji di mesin ini, Windows mengisi properti itu untuk
      classic (node hands-free `BTHENUM\{111E/111F}…`) **dan** LE (node
      `BTHLE\DEV_…`, mis. keyboard AULA 46%, sama dengan Settings). Dicocokkan
      lewat properti alamat (`DEVPKEY_Bluetooth_DeviceAddress`). Dibaca saat
      perangkat baru tersambung, tiap 6 s selama ada yang belum berdata,
      lalu tiap 30 s. Perubahan level memperbarui indikator tanpa alert.
      Referensi: [BlueGauge](https://github.com/iKineticate/BlueGauge)
      (`src/bluetooth/btc.rs`, `ble.rs`), panduan PowerShell
      `Get-PnpDeviceProperty` dari allthings.how. Cek manual:
      `cargo test lists_connected -- --ignored --nocapture`.
  - [ ] Bila perlu: alert "baterai Bluetooth rendah" (mis. ≤ 20%, sekali per
        perangkat), dan level di indikator notch istirahat. Keyboard AULA
        terdeteksi sebagai `Other` (namanya tak dikenali, Appearance LE-nya
        bukan keyboard), jadi ikonnya rune Bluetooth.
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

- [x] **Lirik per kata + font lagu lebih tebal** (2026-10-30).
      Lirik: baris yang sedang dinyanyikan menyala huruf demi huruf seperti
      Apple Music (bagian yang belum: 30% antara redup dan putih; tepi
      lembut 1,6 karakter; huruf yang sudah dinyanyikan naik 1 px). Font:
      lirik SemiBold → ExtraBold, artis Bold → ExtraBold, judul ExtraBold +
      faux-bold (`text_heavy`: dua lapis berjarak 1 px fisik, alpha dibagi
      agar tepi tidak menebal).
      | Pilihan | Dipakai | Alasan |
      |---|---|---|
      | Waktu per kata: hanya enhanced LRC vs perkiraan | **keduanya**: tag `<mm:ss.xx>` bila ada (`Line::words`), selain itu dibagi rata 85% jarak ke baris berikut, maks 0,2 s/karakter | LRCLIB kebanyakan hanya per baris; batas kecepatan mencegah wipe merayap di jeda panjang |
      | Wipe dengan mask bergradasi vs warna per glyph | **per glyph** (`Canvas::glyphs` dengan gaya per karakter) | tanpa alokasi mask per frame |
      | Judul lebih tebal: font Inter Black baru vs faux-bold | **faux-bold** | tanpa unduhan/aset baru (~300 KB) |
      | Frame: loop vsync vs timer | **timer 33 ms** hanya saat lirik tersinkron tampil & lagu jalan (selain itu 100 ms) | ~6% satu core selama notch terbuka dengan lirik (diukur 313 ms CPU / 5 s); nol saat tertutup |
      Diuji: `parses_word_timings`, `lights_up_word_by_word`; di notch nyata
      ("Counting On You") efek mengikuti lagu: "So tel|l me…", "Will you|
      love…", "You feeli|ng…". Catatan: tanpa waktu per kata, kecepatan
      adalah perkiraan; pada baris yang dinyanyikan cepat lalu jeda, nyala
      bisa sedikit tertinggal.

- [x] **Jam di notch diam** (2026-10-29). Seperti status bar iPhone: jam
      di kiri (Inter SemiBold 13), cover + equalizer bergeser ke kanannya;
      baterai dan Bluetooth tetap di kanan. Muat di semua lebar (Narrow +
      baterai mengisi + Bluetooth + musik: kiri berakhir ~99 px, kanan mulai
      ~111 px, termasuk jam 5 karakter). Pill mini (ada jendela maximized)
      tetap tanpa isi.
      | Pilihan | Dipakai | Alasan |
      |---|---|---|
      | Format sendiri (`HH:mm`) vs `GetTimeFormatEx(TIME_NOSECONDS \| TIME_NOTIMEMARKER)` | **`GetTimeFormatEx`** | ikut setelan 12/24 jam pengguna, tanpa AM/PM seperti iPhone ("9:41") |
      | Redraw tiap detik / timer 1 s vs timer ke pergantian menit | **timer satu kali ke menit berikutnya (+50 ms)**, dipasang ulang tiap kali | 1 bangun per menit; tidak menambah frame |
      | Lebarkan notch untuk jam vs pakai ruang kiri yang kosong | **ruang kiri** | ukuran notch diam tidak berubah |
      Lompatan waktu ditangani: `WM_TIMECHANGE` (jam diubah) dan
      `PBT_APMRESUMEAUTOMATIC` (bangun dari sleep) membaca ulang dan
      memasang ulang timer. Pratinjau notch di jendela Settings ikut
      menampilkan jam sungguhan (masuk kunci cache pratinjau).
      Diuji: unit test `clock_is_hours_and_minutes`; log debug di notch
      nyata menunjukkan "11:10 -> 11:11" tepat di pergantian menit.
      Lanjutan bila perlu: pengaturan untuk menyembunyikan jam.

### Prioritas sedang

- [x] **File shelf** (2026-10-30, `shelf.rs`). Halaman ketiga, kini di kiri
      player (lihat "Halaman kiri-kanan"). Seret file ke notch: notch langsung terbuka sebagai
      zona drop biru di halaman shelf. Ubin berisi thumbnail/ikon shell dan
      nama; klik = buka, × = keluarkan dari shelf (file tetap), seret keluar
      = pindah/salin sesuai target (file yang pindah hilang dari shelf
      sendiri), tombol Share (share sheet Windows untuk semua file) dan
      tempat sampah (kosongkan). Roda mouse menggulir file.
      Riset & keputusan:
      | Pilihan | Dipakai | Alasan |
      |---|---|---|
      | Simpan referensi path (boring.notch) vs salin ke folder sendiri (NotchDrop, kedaluwarsa 1 hari) | **referensi** | tanpa ruang disk, instan untuk file besar, "seret keluar = pindah" alami |
      | Tab di atas (boring.notch, NotchNook) vs halaman scroll (DynamicWin) | **halaman ke-3** | notch-woy sudah memakai scroll untuk halaman |
      | Seret keluar: `IDataObject` + `IDropSource` sendiri vs `SHDoDragDrop` + `BHID_DataObject` | **`SHDoDragDrop`** | drop source & gambar drag bawaan shell |
      | Share: `DataTransferManager` di jendela notch vs jendela jangkar | **jangkar 1x1 alpha 0** | diuji: sheet menolak jendela `WS_EX_NOACTIVATE` (diam-diam `Ok`), menerima jendela tool yang bisa aktif; jangkar hilang saat sheet ditutup |
      | Efek drop | **Copy/Link saja** | Move membuat sumber (Explorer) menghapus file |
      Diuji nyata: drop (efek `Copy` diterima sumber, file tetap), seret keluar
      ke target yang memindah (ubin hilang), Share (sheet muncul, jangkar
      hilang setelah ditutup), ×. Ikut diperbaiki: `WM_MOUSELEAVE` palsu
      (setelah drop) diabaikan bila kursor masih di atas notch.
      Referensi: [boring.notch](https://github.com/TheBoredTeam/boring.notch)
      (`components/Shelf`), [NotchDrop](https://github.com/Lakr233/NotchDrop)
      (`TrayDrop.swift`), Microsoft Learn: `IDataTransferManagerInterop`,
      `SHDoDragDrop`, `IShellItemImageFactory`.
  - [ ] Keterbatasan shelf: (1) saat notch berupa pill mini (ada jendela
        maximized), sasaran drop hanya 80×10 px; (2) satu file per seret
        keluar (belum multi-pilih); (3) "Clear" tanpa konfirmasi (hanya
        menghapus referensi, file aman); (4) RAM: tanpa dipakai tidak
        berubah (3,3 MB "Memory", 11,3 MB private bytes setelah trim; thread
        16 → 21: worker ikon + OLE). Setelah Share dipakai, komponen share
        WinRT tetap termuat (+~4 MB private bytes) sampai aplikasi restart.
- [x] **Halaman kiri-kanan** (2026-10-29): halaman notch terbuka kini
      berdampingan, shelf (kiri) | player | Control Center (kanan), digeser
      sejauh lebar notch; titik halaman pindah ke bawah tengah (ruang kanan
      untuk titik dilepas, progress bar dan strip shelf sedikit lebih lebar).
      Gradien cover ikut bergeser (mask kolom halaman player).
      | Input | Perilaku |
      |---|---|
      | Geser samping touchpad / tilt wheel (`WM_MOUSEHWHEEL`) | halaman mengikuti jari, rubber band di ujung, berhenti di halaman |
      | Roda mouse (`WM_MOUSEWHEEL`) | turun = halaman kanan, satu notch satu halaman (seperti daftar menyamping di Windows) |
      | Roda di shelf yang penuh | menggulir file dulu; roda mouse lanjut ke player di ujung, touchpad berhenti |
      | Daftar output terbuka | roda menggulir daftar; geser samping diabaikan |
      Shift+roda untuk shelf dihapus (roda biasa sudah menggulir file).
      Diuji nyata: `WM_MOUSEHWHEEL` sampai ke jendela notch (`NOACTIVATE`),
      ±120 membalik halaman, langkah kecil ±12 menyeret lalu berhenti di
      halaman yang benar.
  - [x] **Gradien cover untuk semua halaman** (2026-10-29). Masalah: gradien
        (lirik terbuka) hanya milik halaman player, jadi saat digeser ada
        tepi keras antara halaman berwarna dan halaman hitam.
        | Pilihan | Hasil | Biaya |
        |---|---|---|
        | Gradien ikut bergeser dengan player (sebelumnya) | tepi keras di tengah notch | mask selebar kanvas per frame selama geser |
        | Tepi gradien dibuat lembut (feather) | tepi tetap terlihat sebagai pita, halaman samping tetap hitam | mask bergradasi per frame |
        | Crossfade: gradien diam, memudar ke hitam di halaman samping | tanpa tepi, tapi halaman samping hitam polos | tanpa mask |
        | **Latar ambient**: gradien diam di belakang semua halaman, 60% di halaman samping | tanpa tepi; modul kaca Control Center dan ubin shelf (isi putih tembus) ikut berwarna, seperti Control Center iOS di atas wallpaper | **tanpa mask (paling murah)** |
        Dipilih latar ambient (`SIDE_PAGE_TINT` di `render.rs`). Pemilih
        output tetap menutup gradien hanya di halaman player. Diuji di
        pratinjau dan notch nyata (lagu diputar, lirik terbuka, geser ke
        Control Center dan shelf): teks dan zona drop tetap terbaca.
  - [x] **Ukuran per halaman** (2026-10-29). Lebar notch terbuka mengikuti
        halaman (`render::open_width`), seperti Dynamic Island yang berubah
        ukuran per aktivitas:
        | Halaman | Lebar | Alasan |
        |---|---|---|
        | Shelf | 400–520, pas dengan jumlah file (`shelf_width`); ubin di tengah | 1–4 file tidak menyisakan ruang kosong; minimal 400 agar petunjuk zona drop muat |
        | Player | 420 (tanpa lirik) / 520 (lirik) | tetap |
        | Control Center | 560 (`CONTROLS_WIDTH`) | modul Now Playing dan Bluetooth sebelumnya memotong nama ("Galaxy Buds..."); kini utuh |
        Pilihan yang dibandingkan: (1) satu ukuran untuk semua (sebelumnya),
        (2) ukuran tetap per halaman, (3) **ukuran per halaman + mengikuti
        isi (shelf)**, dipilih. Saat diseret touchpad, lebar diinterpolasi
        antara dua halaman; halaman ditata tepi ke tepi (jarak geser =
        rata-rata dua lebar), jadi tidak ada celah atau tumpang tindih.
        Tinggi tetap 150: ketiga halaman sudah mengisinya (modul CC sampai
        138, ubin shelf sampai 136, titik halaman di 144). Batas kanvas: 560
        + bahu 2×8 + pantulan pegas (~3,5%) ≤ 591 < 600.
        Catatan: bila kursor berada di tepi Control Center lalu pindah ke
        halaman yang lebih sempit, notch bisa tertutup karena kursor kini di
        luar notch (perilaku wajar, sama seperti hover biasa).
        Referensi: Apple HIG, [Live Activities](https://developer.apple.com/design/human-interface-guidelines/live-activities)
        (ukuran compact/minimal/expanded Dynamic Island).
  - [x] **Animasi shelf: file diseret masuk & dilepas** (2026-10-29).
        Saat file diseret di atas notch (`drop_glow`, spring 0..100):
        zona drop menyala dari abu-abu ke biru dan sedikit membesar (pop
        spring), garis putus-putus berjalan (`dashed` dengan fase dari
        `clock`), ikon panah-ke-baki naik turun. Ubin memudar dulu (0–45%),
        baru teks "Drop to add…" muncul (45–100%), tidak bertumpuk.
        Saat dilepas: notch "menelan" (`DROP_PULSE`, kick lebar/tinggi
        seperti pulsa lagu baru), zona memudar, ubin lama bergeser ke kanan
        dulu (0,28 s), lalu ubin baru jatuh satu per satu (jeda 0,1 s, selang
        0,07 s, 0,5 s, mendarat dengan pantulan `ease_out_back`).
        `Shelf::add` kini mengembalikan jumlah file yang masuk.
        | Pilihan | Dipakai | Alasan |
        |---|---|---|
        | Ubin baru membesar (scale) vs jatuh + pantul (geser y) | **jatuh + pantul** | `Canvas` hanya punya offset; skala butuh transform per ubin dan ikon dirender ulang; jatuh juga lebih cocok dengan "masuk ke baki" |
        | Garis berjalan via timer sendiri vs loop frame | **loop frame** (`is_animating` saat `drop_hover`) | hanya jalan selama drag, ikut vsync (`DwmFlush`) |
        | Waktu: spring vs durasi tetap | spring untuk nyala zona (bisa berbalik di tengah), durasi tetap + stagger untuk kedatangan | kedatangan harus berurutan per ubin |
        Diuji nyata: drag OLE 2 file ke notch (efek `Copy`), tangkapan layar
        saat hover (zona biru, ikon, teks), 140 ms (ubin pertama jatuh, ubin
        lama bergeser), 300 ms (ubin kedua mendarat), 900 ms (selesai). Isi
        shelf pengguna dikembalikan setelah uji.
        Catatan: drop file yang semuanya tidak ada (path rusak) tetap
        melaporkan `Copy` ke sumber walau tidak ada yang masuk (perilaku lama).
    - [x] **Animasi drop ala iOS/macOS** (2026-10-29), menggantikan "ubin
          jatuh": seperti drop di iOS (`UIDropInteraction`: pratinjau drag
          dianimasikan dari jari ke tujuannya) dan macOS, ikon file **terbang
          dari titik kursor saat dilepas** ke slotnya lewat lengkung
          (Bezier kuadrat: menyamping dulu, lalu turun), mulai dari ukuran
          pratinjau drag (1,45x) dan mengecil dengan pegas (`ease_out_back`:
          sedikit di bawah 1x lalu kembali). Beberapa file berangkat sebagai
          tumpukan yang sedikit menyebar, berselang 0,07 s. Saat mendarat
          (70% penerbangan), ubin berkilau sesaat dan cincin cahaya tipis
          menyebar lalu hilang (0,45 s); nama file baru muncul setelah
          mendarat. Ubin lama tetap bergeser memberi tempat (0,28 s).
          | Pilihan | Dipakai | Alasan |
          |---|---|---|
          | Jatuh dari atas (versi sebelumnya) vs terbang dari kursor | **terbang dari kursor** | kontinuitas dengan gerakan pengguna, pola iOS/macOS |
          | Skala ubin penuh (ikon + teks) vs skala ikon saja | **ikon saja**, teks muncul setelah mendarat | teks dirasterisasi per ukuran piksel; skala yang beranimasi akan memenuhi cache glyph |
          | Riak tebal vs tipis | **tipis** (r +16, alpha 0,3) | versi tebal menimpa nama file |
          Posisi kursor dibaca saat drop (`GetCursorPos`, ke px logis
          kanvas); titik awal dijaga cukup rendah agar ikon besar tidak
          terpotong tepi atas. Diuji nyata (drag OLE 2 file): 90 ms ikon besar
          meluncur dari atas, 180 ms melengkung ke slot, 400 ms mendarat +
          riak, 900 ms selesai. Referensi: Apple Developer, [Drag and drop
          (UIKit)](https://developer.apple.com/documentation/uikit/drag-and-drop),
          [UIDropInteractionDelegate](https://developer.apple.com/documentation/uikit/uidropinteractiondelegate).
- [x] **Visualizer dari audio asli** (2026-10-30, `meter.rs`). Equalizer
      notch diam (3 bar) dan alert lagu baru (4 bar) kini mengikuti suara
      yang keluar dari perangkat output default.
      | Pilihan | Dipakai | Alasan |
      |---|---|---|
      | Peak meter `IAudioMeterInformation` vs loopback capture + FFT (Rainmeter AudioLevel) | **peak meter** | satu panggilan COM per frame, tanpa thread/buffer/FFT; pita frekuensi berlebihan untuk 3–4 bar mungil |
      | Meter di thread volume (MTA) + kirim nilai vs dibaca di thread UI | **thread UI**, hanya saat equalizer tampil | tanpa sinkronisasi; nol kerja saat tidak tampil |
      | Pakai `IAudioEndpointVolume` yang dipegang `volume.rs` vs aktivasi sendiri | **aktivasi sendiri** (`Meter`), cek perangkat default tiap 1 s | antarmuka berbeda; objek COM thread volume tidak dibagi ke STA |
      | Semua bar = level yang sama vs bar berbeda | **bar berbeda**: kanal kiri/kanan bergantian, tiap bar tertunda 2 sampel (66 ms) dari sebelumnya, release berbeda (0,12–0,24 s) | ketukan "berjalan" melintasi bar seperti spektrum |
      Level dalam dB (rentang 36 dB), dikurangi 75% rata-rata berjalan
      (1,5 s) supaya master yang keras tidak membuat bar mentok di atas;
      naik cepat (25 ms), turun pelan. Diukur di musik nyata: bar bergerak
      di 0,24–0,74 (rata-rata ~0,5).
      Catatan Microsoft: meter render membaca **sebelum** volume endpoint,
      jadi bar tetap hidup walau volume kecil; meter software bernilai 0
      pada stream mode exclusive. Karena itu bila hening total > 2 s
      sementara lagu "diputar" (exclusive, atau audio di perangkat lain),
      equalizer kembali ke animasi lamanya, dan mengikuti lagi begitu ada
      suara.
      Diuji: `follows_the_music`, `gives_up_on_silence`,
      `loudness_is_in_decibels`; `reads_real_meter` (perangkat nyata,
      puncak 0,1–0,4); `NOTCH_LIVE_METER=1 cargo test preview -- --ignored`
      merender notch diam + alert dari meter nyata (bar berubah antar frame).
      Belum terlihat langsung di layar: saat diuji notch berupa pill mini
      (jendela maximized), yang memang tanpa equalizer.
      Referensi: Microsoft Learn [Peak Meters](https://learn.microsoft.com/en-ie/Windows/Win32/coreaudio/peak-meters),
      [IAudioMeterInformation](https://learn.microsoft.com/en-us/windows/desktop/api/endpointvolume/nn-endpointvolume-iaudiometerinformation);
      [Rainmeter AudioLevel](https://docs.rainmeter.net/manual/plugins/audiolevel/).
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
