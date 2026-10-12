# Rencana Implementasi — Edunex Downloader (`edunexsh`)

> Target: CLI interaktif ala shell untuk menelusuri + mengunduh materi
> kuliah dari **https://edunex.itb.ac.id** (LMS Vue.js bikinan Cognisia,
> login SSO ITB) tanpa buka satu-satu di browser.
> Contoh goal: `cd MA1101-aljabar-linear` → `dl` → semua slide turun.
>
> Bahasa: **Python** (paling sederhana, tanpa butuh performa).
> Dependensi: **`requests` saja** (+ stdlib: `cmd`, `json`, `os`, `pathlib`).
> Tanpa `tqdm`, tanpa DB, tanpa framework.
>
> Dokumen ini ditulis agar agen/sesi lain bisa mengerjakan dari nol
> sampai beres tanpa bertanya ulang. Kerjakan fasenya berurutan.

---

## 0. Konteks hasil survei (fakta, Okt 2026)

- Edunex = **SPA Vue.js** (bundle `/js/app.<hash>.js` + `chunk-vendors`),
  backend API internal (kemungkinan satu host atau subdomain `api`).
  Endpoint pasti **belum dipetakan** — itu kerjaan F0.
- Login: **SSO ITB / email-password** di `/pages/login`. SSO hampir pasti
  tidak bisa diotomatisasi (lihat pelajaran deepshit vs WAF) →
  **auth = session dari browser user** (cookies/token), tool yang memakai.
- Format materi **campuran**: `.pptx`, `.pdf`, dan item **viewer**
  (`slide-reader`, `pdf-reader` di bundle). Viewer kemungkinan memecah
  slide jadi gambar/halaman → strategi unduh di §4.
- Ada aplikasi mobile `com.cognisia.edunex` sebagai **jalur cadangan**
  kalau API web buntu (APK bisa di-decompile untuk baca endpoint).
- Bundle JS yang relevan (nama file berubah tiap deploy, cari ulang
  pola namanya): `course/read.*.js`, `course/enroll.*.js`,
  `answer-reader/slide-reader.*.js`, `content/reader/pdf.*.js`,
  `pages/login.*.js`, `app.*.js`, `chunk-vendors.*.js`.

---

## 1. Desain UX (kontrak, jangan diubah sembarangan)

REPL dengan prompt menunjukkan posisi, perintah ala shell:

```
$ python edunex.py
edunex> login
paste session (satu baris JSON atau path file): {...}
ok, halo NAMA (7 matkul)
edunex> ls
MA1101-aljabar-linear/   FI1101-fisika-dasar/   ...
edunex> cd MA1101-aljabar-linear
edunex:MA1101-aljabar-linear> ls
Minggu-01/  Minggu-02/  Tugas/  UTS/
edunex:MA1101-aljabar-linear> cd Minggu-01
edunex:MA1101-aljabar-linear/Minggu-01> ls
slide-vektor.pptx   latihan.pdf   kuis-1 [viewer]
edunex:MA1101-aljabar-linear/Minggu-01> dl
[1/3] slide-vektor.pptx ......... ok (2,1 MB)
[2/3] latihan.pdf ............... ok (340 KB)
[3/3] kuis-1 .................... SKIP viewer-only (lihat `dl --include-viewer`)
edunex:MA1101-aljabar-linear/Minggu-01> cd ../.. && exit
```

Spesifikasi perintah:

| Perintah | Perilaku |
|---|---|
| `login` | minta session (paste JSON **atau** path file) → verifikasi ke server (`whoami` / list matkul) → simpan ke `~/.edunexsh/session.json` (mode `0600`). Gagal verifikasi = jangan simpan, tampilkan error jelas. |
| `ls [path?]` | root = daftar matkul ter-enroll (dengan `/` di akhir). Dalam matkul = modul + file. File viewer ditandai `[viewer]`. Tanpa argumen = posisi sekarang. |
| `cd <path\|..\|/\|~>` | pindah posisi. Path relatif/absolut, dukung `..`. Tujuan tidak ada = error + daftar yang mirip (bantu typo). |
| `pwd` | cetak posisi virtual. |
| `info [path?]` | detail item: judul, tipe, ukuran (kalau diketahui), URL sumber, status viewer/bukan. |
| `dl [--ext pptx,pdf] [--include-viewer] [path?]` | unduh **semua file di posisi sekarang** (rekursif ke subfolder, struktur folder dipertahankan). Default: lewati viewer-only dengan catatan. `--ext` = saring ekstensi. File yang sudah ada dengan ukuran sama = SKIP (resume murah). Gagal satu file = lanjut + rekap error di akhir, jangan abort. |
| `get <file>` | unduh satu file di posisi sekarang. |
| `whoami` / `status` | tampilkan user + umur session. |
| `help [cmd]` | bantuan per perintah. `exit`/`quit`/Ctrl-D keluar. |

Aturan UX: output ringkas per baris, progress per file satu baris
(`[i/n] nama ... ok / SKIP alasan / FAIL alasan`), tidak ada spinner
aneh-aneh, semua error berbahasa Inggris sederhana.

---

## 2. Arsitektur (sengaja minimal)

```
edunex.py            ← satu file, REPL (stdlib `cmd`) + perintah di §1
  ├─ session.py      ← muat/simpan ~/.edunexsh/session.json, header auth
  ├─ api.py          ← SEMUA HTTP ke Edunex (satu tempat, gampang di-mock test)
  │     list_courses() -> [Course]
  │     list_modules(course_id) -> [Module]      (rekursif bila perlu)
  │     list_items(module_id) -> [Item]          (file | viewer | link | tugas)
  │     file_url(item) -> (url, filename, size?) (resolve redirect/signed URL,
  │                                               JANGAN download di sini)
  │     download(url, dest_path)                 (stream + resume + retry)
  │     me() -> user info (untuk verifikasi login)
  └─ vfs.py          ← model pohon virtual: Course/Module/Item + resolve path
```

Kenapa dipisah walau kecil: supaya sesi baru bisa unit-test `vfs.py`
tanpa login, dan `api.py` bisa diganti mock. Total target < 800 baris.

Aturan keras:

- Semua URL/base/API key hanya di `api.py`. Tidak ada URL tercecer di REPL.
- Tidak ada kredensial (password/token) di repo, di log, atau di commit.
  `session.json` di home user, **bukan** di folder repo. `.gitignore` mencakupnya.
- Setiap request HTTP: `User-Agent` browser biasa, timeout 60 dtk,
  retry 3x + backoff untuk error jaringan/5xx, **jeda 0,5–1 dtk antar
  request** (sopan, hindari rate-limit), stop dan laporkan jelas untuk 401/403
  ("session mati, ulangi `login`").
- Download: streaming (`stream=True`, chunk 1 MB), tulis ke
  `<nama>.part` lalu rename atomik saat selesai (file setengah tidak
  pernah terlihat utuh). Resume: kalau server dukung `Range`, lanjutkan;
  kalau tidak, ulangi dari awal HANYA bila ukuran beda, samakan ukuran = SKIP.

---

## 3. F0 — Riset protokol (tanpa login, bisa dikerjakan duluan)

Keluaran: **`edunex-protocol.md`** + skrip `curl` bukti per endpoint.
Jangan coding F1 sebelum F0 beres.

1. Ambil HTML `/pages/login`, catat URL `app.<hash>.js` + `chunk-vendors`
   terbaru (hash berubah tiap deploy — selalu ambil ulang).
2. Unduh bundle JS ke `/tmp` (jangan ke repo), grep:
   - base API: `baseURL`, `axios.create`, `apiUrl`, `API_URL`,
     `edunex.cognisia.id`, `/api/`, `/v1/`, `/webhook`
   - auth: `login`, `sso`, `token`, `refresh`, `Authorization`, `Bearer`,
     `Set-Cookie`, `XSRF`, `csrf`
   - matkul: `course`, `enroll`, `my-course`, `dashboard`
   - isi: `module`, `content`, `material`, `attachment`, `slide`,
     `reader`, `learning-plan`, `syllabus`
   - unduh: `download`, `attachment`, `file`, `signed`, `viewer`
3. Untuk tiap endpoint kandidat, catat: method, path, contoh body,
   contoh respons (sanitasi!), header auth yang dipakai
   (cookie? Bearer? keduanya?).
4. Uji yang bisa diuji tanpa login (`curl` + amati 401 vs 404 vs 200 —
   401 = endpoint ada tapi butuh auth, itu kabar baik).
5. Susun peta alur viewer: dari item slide → request apa → URL halaman/
   gambar dari mana (ini bekal §4). Cukup dari baca JS, belum eksekusi.
6. Tulis `edunex-protocol.md`: tabel endpoint + contoh mentah +
   bagian "belum tahu" yang jujur.

Kriteria beres F0: orang lain bisa baca `edunex-protocol.md` dan tahu
persis request apa untuk login-check, list matkul, isi modul, dan unduh
file — atau tahu persis bagian mana yang masih gelap.

---

## 4. F1 — Auth (butuh user sekali)

1. Implementasi `session.py` + perintah `login` sesuai §1.
   Format session file (JSON): `{"cookies": {...}, "token": "..."|null,
   "user_agent": "...", "saved_at": 123}` — dukung keduanya karena hasil
   F0 bisa cookie-only, Bearer-only, atau campuran.
2. Verifikasi = panggil endpoint paling ringan hasil F0 (`me()` atau list
   matkul). Gagal = jangan simpan.
3. **Minta user**: pandu ambil session dari browser yang sudah login
   (DevTools → Application → Cookies `https://edunex.itb.ac.id` +
   Local Storage; tulis langkah persis sesuai temuan F0 — cookie-based
   atau Bearer-based). User paste ke `login`, tool verifikasi.
4. Dokumentasikan langkah itu di README (copy-pasteable, ≤ 5 langkah).

Kriteria beres F1: `login` → `ls` menampilkan matkul asli user.

---

## 5. F2 — REPL + mesin unduh (inti)

1. `vfs.py`: node `Root/Course/Module/Item`; `resolve(path)` dukung
   absolut/relatif/`..`; `ls` tampilan; bedakan direktori (`/` akhir)
   vs file vs `[viewer]`. Unit-test dengan data palsu (tanpa login).
2. REPL (`edunex.py`, stdlib `cmd`): semua perintah §1. Tab-completion
   untuk nama matkul/modul kalau murah (stdlib `readline`); kalau ribet,
   boleh skip — catat sebagai nice-to-have.
3. `dl`:
   - kumpulkan daftar file rekursif dari posisi sekarang (tampilkan
     rencana dulu: `N file, ~X MB, M viewer-only dilewati` + lanjut;
     tanpa konfirmasi interaktif — CLI boleh langsung jalan, tapi
     rencanakan dulu di output).
   - struktur folder lokal = struktur virtual,
     sanitasi nama (`/ \ : * ? " < > |` → `_`, trim, batasi 120 char).
     Simpan peta `remote_id → path lokal`? Tidak perlu — path deterministik
     dari judul + id pendek bila judul kembar (`<judul> (<id6>)`).
   - filter `--ext`, SKIP-bila-ukuran-sama, `.part` + rename atomik,
     rekap akhir (`ok / skipped / failed` + daftar gagal).
4. Uji live: 1 modul kecil dulu → cek file terbuka normal → baru 1 matkul
   penuh (algeo). Bandingkan jumlah file vs `ls`.

Kriteria beres F2: dari REPL, `cd` ke matkul algeo + `dl` menghasilkan
semua file non-viewer di disk dengan nama/folder benar.

---

## 6. F3 — Viewer + polish (penutup)

1. Viewer: untuk tiap item viewer, kejar URL sumber asli dulu
   (lampiran tersembunyi? PDF konversi? endpoint export?). Kalau dapat =
   unduh seperti file biasa. Kalau mentok = `dl` mencatat
   `SKIP viewer-only`; `dl --include-viewer` mengunduh gambar per-halaman
   (`<judul>/page-001.jpg`…) **hanya** bila polanya sudah dipetakan di F0.
   Jangan pura-pura berhasil.
2. Polish: progress per file (`xx%` + KB/s sederhana, tanpa lib),
   retry/backoff, jeda sopan, `--ext`, rekap error, `info` lengkap.
3. README final (Inggris sederhana): quickstart 3 langkah, contoh sesi
   REPL asli (copy dari run live), tabel perintah, troubleshooting
   (401 → `login` ulang, 429 → tunggu, file gagal → ulangi `dl`).
4. Uji akhir: hapus folder hasil → `dl` ulang dari nol di matkul algeo →
   semua file utuh + bisa dibuka.

Kriteria beres F3 (= DEFINITION OF DONE keseluruhan): user mengetik
`cd <matkul-algeo>` + `dl` dan mendapat semua slide tanpa membuka
browser, dengan rekap jujur untuk item yang dilewati.

---

## 7. Risiko & batasan (tulis jujur di README juga)

- **Session SSO kedaluwarsa** → 401 sewaktu-waktu; desain sudah siap
  (`login` ulang 30 detik). Jangan simpan password user di mana pun.
- **Signed/expiring URL** → resolve URL tepat sebelum unduh tiap file,
  jangan cache URL antar sesi.
- **Struktur matkul beda-beda** (dosen bebas menyusun) → VFS harus
  generik (rekursi modul), jangan hardcode "Minggu-XX".
- **Rate-limit / blokir** → jeda + retry + pemakaian pribadi wajar.
  Ini materi kuliah user sendiri yang ter-enroll; tetap sopan ke server.
- **Viewer-only tanpa sumber** → laporkan sebagai SKIP, bukan gagal diam-diam.
- **API berubah** (bundle hash baru tiap deploy) → semua endpoint di
  satu file `api.py` + `edunex-protocol.md` sebagai peta.

---

## 8. Yang TIDAK dikerjakan (scope cut, sengaja)

- Otomatisasi login SSO/password (diblokir di level SSO; pola deepshit).
- GUI / web UI / bot Telegram.
- Download video kuliah (boleh jadi follow-up, bukan goal).
- Unduh matkul yang tidak ter-enroll / milik orang lain.
- Paralelisme agresif (cukup sekuensial + jeda; user tidak butuh performa).

---

## 9. Lampiran: titik awal yang sudah diketahui

- Root: `https://edunex.itb.ac.id/` → SPA, `<div id="app">`
- Login: `https://edunex.itb.ac.id/pages/login` (email/password + SSO)
- Contoh kursus publik: `https://edunex.itb.ac.id/public/courses/27724`
  (ED0006 Panduan Edunex — bisa dipakai untuk intip struktur tanpa login)
- Platform: Cognisia (`edunex.cognisia.id` asset host, app mobile
  `com.cognisia.edunex` sebagai cadangan)
- Pola repo rujukan: `../deepseek_RE` (deepshit) untuk gaya repo Rust —
  **tidak** dipakai di sini (proyek ini Python, repo baru terpisah).
- Repo + bahasa: repo Git baru, README Inggris sederhana,
  kode + pesan CLI Inggris (pelajaran dari deepshit: Inggris dari hari 1).
