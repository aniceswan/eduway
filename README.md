# eduway — download matkul Edunex tanpa ribet

![eduway icon](assets/icon-256.png)

`eduway` itu aplikasi buat menjelajah + download semua materi kuliah dari
[Edunex ITB](https://edunex.itb.ac.id) langsung dari laptop — tanpa buka
satu-satu di browser, lewat terminal interaktif (CLI) yang ringan.

---

## Install (pilih caramu)

**Syarat sekali saja: login SSO.** Selebihnya token tersimpan aman di
`~/.eduway/session.json` dan dipakai terus.

### Linux 🐧

Paling gampang — file `.deb`:

```bash
# download eduway-Linux.deb dari halaman Releases, lalu:
sudo dpkg -i eduway-Linux.deb
eduway
```

Atau tanpa sudo (script install):

```bash
EDUWAY_REPO=aniceswan/eduway ./install.sh
```

Yang terjadi: binary masuk `~/.local/bin`, shortcut + ikon masuk menu aplikasi.

### Windows 🪟

Download **`eduway-Windows-Setup.exe`** dari halaman Releases → klik next-next
selesai. Shortcut `eduway` muncul di Start Menu.

### macOS 🍎

Download **`eduway-macOS.dmg`** dari halaman Releases → buka → seret ke
Applications. Catatan jujur: belum notarized Apple, jadi klik-kanan →
Open di pertama kali.

> Semua file rilis dibuat otomatis GitHub Actions tiap versi baru —
> lihat tab **Releases** di repo ini.

### Dari source (butuh Rust)

```bash
git clone https://github.com/aniceswan/eduway && cd eduway
cargo build --release
./target/release/eduway
```

Uninstall: hapus binary + `rm -rf ~/.eduway ~/Downloads/eduway`.

---

## Pakai (3 langkah)

1. **Buka app** (`eduway` di terminal, atau klik ikon eduway).
2. **Login** — copy token dari browser (sekali saja):
   login di `https://edunex.itb.ac.id` → F12 → Console →
   paste `copy(localStorage.getItem('auth'))` → Enter.
3. **Download** — `ls` → `cd active/<matkul>` → `dl`. File mendarat di
   `~/Downloads/eduway/<matkul>/<modul>/...`.

```text
$ eduway
eduway> login
paste session (copy(localStorage.getItem('auth'))) or file path: {"accessToken":"..."}
ok, halo (ketik `ls` untuk daftar matkul)
eduway> ls
active/   inactive/   all/
eduway> cd active/IF2120-Probability-and-Statistics
eduway:active/IF2120-... > dlall IF2120-Probability-and-Statistics
[1/8] Minggu-01/Peluang.pptx ... ok (4.7 MB)
...
done: ok=8 skipped=0 failed=0
```

---

## Perintah CLI

| Perintah | Artinya |
|---|---|
| `login` | paste token `auth` dari browser (sekali saja, lalu tersimpan) |
| `ls [path?]` | root = tab `active/ inactive/ all/`. Di tab = matkul. Di matkul = modul. Di modul = file (`[viewer]` = bukan file) |
| `cd <path\|..\|/\|~>` | pindah. `cd inactive` bisa dari mana aja. Typo dikasih saran |
| `pwd` | posisi sekarang |
| `info [path?]` | detail: judul, tipe, ukuran, URL, viewer atau bukan |
| `dl [--ext=pdf,pptx] [--include-viewer] [path?]` | download semua di posisi ini, rekursif. Gagal 1 file ga gugurin yang lain; ada rekap akhir |
| `dlall <matkul>` | download 1 matkul FULL (semua modul) dari mana aja. Nama boleh setengah, auto-cari di semua tab |
| `get <file>` | download 1 file di folder ini |
| `whoami` / `status` | info user + umur session |
| `logout` | hapus session + cache (mulai dari nol lagi) |
| `refresh [--all]` | buang cache, tarik ulang daftar matkul |
| `help [cmd]` | bantuan. `exit`/`quit`/Ctrl-D keluar |

Tips: pencet `Tab` untuk auto-complete (command, nama matkul/modul/file, flag).

### Arti tulisan SKIP (biar ga bingung)

- `SKIP sudah ada (ukuran sama)` = file itu **sudah ada** di disk → ga diulang. Aman `dl` berkali-kali.
- `SKIP viewer-only (tidak ada file)` = itemnya bukan file (link/meet/teks) → dilewati jujur, bukan error. Mau ikut? `dl --include-viewer` (kesimpan `.txt`).
- `done: ok=X skipped=Y failed=Z` = rekap. Yang `failed` dirinci per file — ulangi `dl` untuk itu aja.

---

## Cara login (detail)

`login` tanpa argumen minta kamu paste nilai `auth` dari DevTools browser.
Bisa juga `login /path/ke/session.json`, atau `login <email> <password>`
(password dipakai sekali, tidak disimpan). Token diverifikasi ke server
dulu baru disimpan — gagal = tidak disimpan. Detail protokol ada di
[`edunex-protocol.md`](edunex-protocol.md) (endpoint + bukti curl).

---

## FAQ

**Dataku aman?** Token cuma di `~/.eduway/session.json` (mode `0600`, cuma
kamu yang bisa baca). Password ga pernah disimpan di mana pun. Logout =
file-nya dihapus.

**Fileku masuk ke mana?** `~/Downloads/eduway/<matkul>/<modul>/<file>`.
Ubah via env `EDUWAY_DIR=/path/lain`.

**Kok nama file ga ada ekstensinya?** Server ngasih URL tanpa nama file —
eduway nebak tipe aslinya dari isi file (magic bytes + Content-Type, ngintip
isi zip buat bedain pptx/docx/xlsx) lalu rename otomatis. Terbukti:
`Variable random` → `Variable random.ppt` (beneran PowerPoint lawas).

**Matkulku kok beda sama di web?** Samain filter webnya: tab `active` =
`filter[is_active][is]=1` persis. `refresh` kalau baru ganti matkul.

**Video kuliah bisa?** Belum (di luar scope, lihat Limits).

---

## Buat developer

```bash
cargo test   # 17 unit test, tanpa login
cargo build --release
```

Struktur (`src/`): `main.rs` (REPL + Tab-completion) · `api.rs` (semua HTTP di
satu file) · `session.rs` (`~/.eduway`) · `vfs.rs` (pohon virtual).

Rilis: `git tag v0.1.0 && git push origin v0.1.0` → Actions build
Linux (.deb + tar.gz), Windows (Setup.exe + zip), macOS (.dmg + tar.gz).

## Limits (jujur)

- Cuma matkulmu yang ter-enroll. Bukan alat bobol.
- Tanpa video (HLS), tanpa unduh paralel agresif (sekuensial + jeda sopan).
- Homepage/assignment/exam di web ga dipetakan — scope-nya file matkul.
