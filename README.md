# Aura for Windows — Fase 0 (scaffold + pill)

Port Rust dari Aura (Mac) ke Windows. UI Aura dipertahankan (ukuran, layout,
warna, animasi, teks Inggris). Render Direct2D, window Win32 (resep Nusa),
tanpa webview.

> **Catatan jujur:** Eve tidak bisa compile Rust di VM ini (tidak ada
> cargo/rustc). Kode ini **belum terverifikasi compile**. Build pertama di
> PC Windows adalah tempat error signature/API diperbaiki — kirim output
> `cargo build` ke Eve, nanti diperbaiki iteratif (workflow yang sama
> seperti build Swift di Mac).

## Build & jalankan (di PC Windows)

```powershell
# Di folder repo aura-windows:
cargo build
# Jalankan dari Explorer (double-click) ATAU dari Terminal:
.\target\debug\aura.exe
# Release (untuk .exe yang dibagikan):
cargo build --release
.\target\release\aura.exe
```

Karena `#![windows_subsystem = "windows"]`, tidak ada jendela console saat
dijalankan dari Explorer (benar untuk app pill). Kalau butuh log, jalankan
dari Terminal — output `println!` (kalau ada) tidak tampil untuk subsystem
windows; verifikasi Fase 0 adalah visual (pill muncul), bukan log.

## Yang harus terjadi (verifikasi Fase 0)

1. Pill hitam kecil (140×32) muncul di tengah atas layar, tidak menabrak
   taskbar (kalau taskbar di atas, pill di bawahnya; kalau taskbar di bawah
   (default), pill di Y=0 work area).
2. Hover pill → membesar jadi 180×32 (compact) dengan animasi halus
   (0,35 dtk easeOutCubic), menampilkan jam (HH:MM, update tiap detik) +
   waveform dekoratif di kanan.
3. Keluar hover (kursor keluar pill) → mengecil kembali ke 140×32 (Zen).
4. Klik pill → membuka kartu placeholder 520×180 ("Aura" + "Fase 0 — kartu
   musik menyusul di Fase 1"). Klik lagi di kartu → kembali ke compact.
5. Ikon tray "Aura" muncul di system tray (kanan bawah). Klik kiri/kanan
   ikon → menu (Status Deteksi (Fase 4) + Quit). Quit menutup app + ikon
   tray hilang (tidak nyangkut).
6. Launch dari Explorer (double-click .exe) jalan, bukan hanya dari
   Terminal. Tidak ada jendela console.
7. DPI: di 100% dan 150%, ukuran fisik pill terlihat sama (tidak jadi
   sangat kecil/besar). Kalau tidak, kirim screenshot + nilai DPI.

## Troubleshooting

- **Pill tidak muncul sama sekali:** cek Task Manager → apakah `aura.exe`
  jalan? Kalau jalan tapi pill tidak kelihatan, kirim screenshot layar
  penuh + posisi taskbar (atas/bawah) + jumlah monitor.
- **Dua pill / pill ganda:** berarti dua instance jalan (mutex gagal).
  Tutup semua via Task Manager, jalankan satu saja, laporkan.
- **Crash saat launch:** kirim output Terminal (kalau dijalankan dari
  Terminal) atau file crash Windows (Event Viewer → Windows Logs →
  Application, cari `aura.exe`).
- **Error `cargo build`:** kirim SELURUH output error (bukan hanya baris
  pertama). Error signature windows-rs 0.58 adalah yang paling mungkin
  di build pertama — Eve perbaiki dari pesan error persis itu.

## Deviasi dari plan (jujur, Fase 0)

- **Renderer:** plan menyebut D2D + DComp + DWrite. Fase 0 memakai
  **D2D HWND render target + SetWindowRgn** (rounded rect) untuk bentuk
  pill, bukan DComp visual tree. Alasan: lebih sederhana + lebih andal
  untuk build pertama; hasil visual untuk pill (rounded rect, fill, teks)
  identik. Animasi Aura digerakkan timer + redraw (seperti Nusa), bukan
  keyframe DComp, jadi DComp tidak esensial. Migrasi DComp (kalau perlu)
  adalah polish Fase 5.
- **NOREDIRECTIONBITMAP tidak dipakai di Fase 0** (itu resep khusus
  DComp; tanpa DComp, flag itu akan merusak rendering). Window memakai
  NOACTIVATE | TOOLWINDOW | TOPMOST saja.
- **Single instance:** mutex `Aura.SingleInstance` (seperti Nusa).
  Instance kedua keluar diam-diam (tidak membawa pill pertama ke depan
  dulu — itu polish, bukan perilaku inti).

## Struktur

- `src/main.rs` — entry (windows subsystem) → `app::run()`.
- `src/app.rs` — App + WndProc + message loop + timer + mouse + tray callback.
- `src/state.rs` — IslandState (Zen/Compact/Expanded) + ukuran logis Aura.
- `src/animator.rs` — easeOutCubic 0,35 dtk.
- `src/renderer.rs` — Direct2D + DirectWrite (pill/kartu placeholder).
- `src/window.rs` — work area, DPI, posisi top-center, region rounded rect.
- `src/tray.rs` — system tray icon + popup menu.

## Berikutnya (Fase 1)

GSMTC sensor + kartu musik 520×180 (cover, progress, kontrol) + lirik
700×180 (LRCLIB). Fondasi Fase 0 (window/render/timer/tray) dipakai ulang,
tidak ditulis ulang.
