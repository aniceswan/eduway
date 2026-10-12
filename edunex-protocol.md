# Edunex Protocol (F0 — surveyed Oct 2026, no login)

Source: HTML + JS bundles from `https://edunex.itb.ac.id` (SPA, same shell for `/` and `/pages/login`).
Bundle hashes at survey time (change every deploy — re-fetch): `app.79bf84f4.js`, `chunk-vendors.624dabd4.js`.
Survey date bundle `Last-Modified: Tue, 28 Apr 2026`. App version `eav2.0.20`.

## 1. Hosts (from `VUE_APP_*` in chunk-vendors)

| Key | Value |
|---|---|
| `VUE_APP_BASEURL` | `https://edunex.cognisia.id` |
| `VUE_APP_API_BASEURL` | `https://api-edunex.cognisia.id` |
| `VUE_APP_STORAGE_API_BASEURL` | `https://api-edunex.cognisia.id/storage` |
| `VUE_APP_CDN_BASEURL` | `https://cdn-edunex.itb.ac.id` |
| `VUE_APP_HLS_API_URL` | `https://api-edunex.cognisia.id/hls` |
| `VUE_APP_SSO_LOGIN_URL` | `https://sso-edunex.itb.ac.id` |
| frontend | `https://edunex.itb.ac.id` (`<div id="app">`, Vue router) |
| login page | `https://edunex.itb.ac.id/pages/login` |
| example public course (frontend only) | `https://edunex.itb.ac.id/public/courses/27724` |

Axios instance (`bb36`): `baseURL=https://api-edunex.cognisia.id`, `Content-Type: application/json`.
Request interceptor: `Authorization: Bearer <accessToken>` where `accessToken` comes from
`localStorage key "auth"` = `{accessToken, refreshToken, expirationDate}`.
CDN helper `p(e)`: if `e` is not `http(s)://...`, prefix `https://cdn-edunex.itb.ac.id/`.

Query builder `se["e"](path, {filter, sort, include, pagination})`
(JSON:API style, observed in app bundle):

```
GET <path>?include=a,b&filter...&sort=...&page[limit]=N&page[offset]=M
```

Response parser `se["d"]`: JSON:API `{data, included, meta}` → flat objects
(`{id, type, ...attributes, <relations>}`); course detail additionally splits
`included.modules` + `included.activity` (merged into one module list).

All HTTP must send browser `User-Agent`, timeout 60s.

## 2. Auth

| # | Method + path (base `https://api-edunex.cognisia.id`) | Body / notes | Proof |
|---|---|---|---|
| A1 | `POST /user/auth/login` | `{username, password}` → `{access_token, refresh_token, expires_in}` (inferred from store `user/login`; empty body → `400 {"error":"Invalid Password"}`) | `curl -X POST .../user/auth/login -d '{"username":"","password":""}'` → 400 |
| A2 | `GET /login/me` | Bearer required. This is the cheapest login-check (`whoami`). No auth → `401 Unauthorized.` | `curl .../login/me` → 401 |
| A3 | SSO (browser only, NOT automatable) | Login page shows `sso_url=https://sso-edunex.itb.ac.id`, `google_url`, `siteInfo.sso_url`. Flow: `GET https://sso-edunex.itb.ac.id/switch/<btoa(...)>` → `302` to `login.microsoftonline.com/.../oauth2/v2.0/authorize?client_id=296f7a9b-...` → callback `/v2/login` → frontend routes `/webhook/sso`, `auth-callback`. | `curl -I https://sso-edunex.itb.ac.id/` → 302 microsoftonline |

Token storage (web): `localStorage "auth"` + `localStorage "userInfo"`.
`isLoggedIn = auth && loginInfo.verified && not expired`.
Tool equivalent (`session.json`, see F1): store `{access_token, refresh_token, expiration_date, user_agent, saved_at}`.

## 3. Courses (all require Bearer — 401 without)

| # | Method + path | Params / include | Notes / proof |
|---|---|---|---|
| C1 | `GET /course/enrolls/courses` | `?include=...&page[limit]=N&page[offset]=M` (built by `se["e"]`) | **List enrolled courses** (student home). `curl` → `401 {"data":[],"error":{"code":401,"message":"Unauthenticated."}}` = endpoint exists |
| C2 | `GET /course/enrolls/courses/{id}` | `?include=...` | Enrolled-course detail |
| C3 | `GET /course/courses/{id}` | `?include=modules,modules.activities,lecturer,lecturer.user,assistant,class` | **Course detail + modules**. Store `fetchCourseById`. `curl /course/courses/27724` → 401 (exists) |
| C4 | `GET /course/courses` | `filter/sort/include/pagination`, always adds `include=modules` | Lecturer/admin course list (not needed for student downloader) |
| C5 | `POST /course/enrolls/take` | `{data:{attributes:{course_id, token, class_id}}}` | Enroll (not needed) |
| C6 | `POST /course/enrolls/leave`, `POST /course/enrolls/kick` | | Leave/kick (not needed) |
| C7 | `GET public/courses/explore` | `{include:"lecturer,lecturer.user", filter[is_active][is]=1, sort=-updated_at, page[limit]=4, ...}` | Labelled "public" but **still 401 without token** (tested). So no unauthenticated course listing |
| C8 | `GET /public/courses` (+ `include=modules,lecturer.user`, pagination) | V1 variant `fetchCoursesPublicV1` | Same: 401 without token in our test |
| C9 | `POST /public/view/course/{id}` | `{courseId, campus, roles, code, name, province, is_verify, ip_address}` | View logging (not needed) |

Open (no auth) — useful as connectivity check only:

| path | result |
|---|---|
| `GET /public/version?domain=edunex.itb.ac.id` | `200 {"domain":"edunex.itb.ac.id","version":"eav2.0.20"}` |
| `GET /time` | `200 {"carbon_now":...,"server_time":...,"time_zone":"Asia/Jakarta"}` |
| `GET /` (API root) | `404 {"message":"no Route matched with those values"}` |

## 4. Modules / activities

Course detail (C3) returns `data + included: {modules, activity}`; client merges them:

```js
modules = {...included.activity, ...included.modules}
dispatch("module/setList", modules)
```

So VFS level 1 = modules dict, each module has `activities` relation (sub-items/lessons).
Single-module fetch: `GET /course/modules/{id}` (delete/patch variants exist but lecturer-only).

## 5. Contents (files / viewer items)

Core student flow:

```
GET /course/contents/?filter[contentable_id][is]={MODULE_OR_ACTIVITY_ID}
                    &filter[contentable_type][is]={Type}
                    &sort=index&page[limit]=0
```

- `contentable_type` mapping observed: `"activity" → "activities"`, course text uses
  `App\Models\Courses` (escaped in JS as `App\\Models\\Courses`).
- Text-of-course special case: `GET /course/contents?filter[contentable_id][is]={courseId}&filter[type][is]=text&filter[contentable_type][is]=App\Models\Courses&sort=index`.
- Single item: `GET /course/content/{id}` and `GET /course/contents/{id}` (both seen; `getContentById` uses singular).
- **Verified live**: module contents = `contentable_type=App\Models\Modules`
  (e.g. module `456820` → 1 row `Peluang`/ppt); other spellings return empty.
- Lecturer write (not needed): `POST /course/contents`, `PATCH /course/contents/{id}`, `POST /course/contents/copy|move`, `PATCH .../restore`, `DELETE /course/contents/{id}`.

Content object fields (from add/updateItem + render code):

```
{id, index, name, contents_type, content, size, mime, contentable_id, contentable_type}
```

`contents_type` → reader component map (from `CoursePreviewPopup.contentReaders` +
chunk names `answer-reader/slide-reader.0368ecd4.js`, `content/reader/pdf.00dbc97f.js`):

| `contents_type` | Reader | Downloadable? |
|---|---|---|
| `text`, `html` | inline text | no (viewer-only) |
| `pdf` | `ContentPDFReader` (pdfjs, `btn Download pdf file`, `href=file`) | **yes, `content` = file URL** |
| `ppt` | `ContentSlideReader` (`iframe https://view.officeapps.live.com/op/embed.aspx?src=<encodeURIComponent(file)>` + `btn Download PPT file`, `href=file`) | **yes, `content` = file URL** |
| `video`, `audio` | video/audio player (`download` attr present) | yes if `content` is file |
| `url` | link reader | only if it points to a file (else viewer-only) |
| `google-meet`, `zoom`, `ms-teams`, `cloudx`, `webex`, `conference` | conference reader (`Join Conference` btn) | no |
| `scorm` | scorm reader | no |
| (unknown) | `ContentFileReader` fallback (`DefaultTaskAnswerReader`, `href=file Download`) | try as file |

`filesize_limit = 1e7` (10 MB) in slide reader; `fullcontent.size || 0`.

## 6. File download

- Direct: `content` field IS the URL (absolute `https://...` or CDN-relative → prefix `https://cdn-edunex.itb.ac.id/`).
- Storage API (seen in `course/enroll` chunk): `window.open("https://api-edunex.cognisia.id/storage/files/download?file=" + t)`. `t` = stored file path. Without auth this returns `500 Call to undefined function ... str_starts_with()` — endpoint exists, needs valid `file` param + auth.
- **Verified live**: real file URLs are Azure Blob **signed URLs**
  (`https://edunexcontentprodhot.blob.core.windows.net/edunex/...?sv=...&sig=...`).
  They MUST be fetched **without** the `Authorization` header (Azure answers
  400 otherwise) — the tool only sends Bearer to `*.cognisia.id` / `*.itb.ac.id`.
- Audio player also uses `window.open(source, "download")`.
- Signed/expiring URLs assumed (per doc §7): **resolve URL immediately before each download, never cache across sessions**.

## 7. curl evidence (no login, UA = Chrome 120)

```
GET /login/me                                        → 401 "Unauthorized."
GET /course/enrolls/courses                          → 401 {"data":[],"error":{"code":401,"message":"Unauthenticated."}}
GET /course/courses/27724                            → 401 (same body)
GET /course/contents/1  /course/content/1            → 401 (same body)
GET /public/courses/explore?...                      → 401 (same body)  # "public" still needs Bearer
GET /public/version?domain=edunex.itb.ac.id          → 200 {"domain":"edunex.itb.ac.id","version":"eav2.0.20"}
GET /time                                            → 200 {...Asia/Jakarta...}
POST /user/auth/login {"username":"","password":""}  → 400 {"error":"Invalid Password"}
GET /storage/files/download?file=test                → 500 (endpoint exists, bad param)
GET https://sso-edunex.itb.ac.id/                    → 302 → login.microsoftonline.com/.../oauth2/v2.0/authorize?...
GET https://cdn-edunex.itb.ac.id/                    → 200
```

Conclusion: **401 = endpoint exists, needs Bearer**. 404 = not exist (API root).

## 8. What is still dark (honest)

1. Exact `include=` needed for `C1` student list (web passes `include: []` by default; course detail needs the long include in §3 C3 — that one is confirmed).
2. Exact `contentable_type` string for module/activity contents in student flow (`activities` vs `modules` vs `App\Models\...` variants) — must confirm with one authenticated run: try `activities` first, then `modules`.
3. Whether `content` for pdf/ppt is absolute URL, CDN-relative path, or storage `file=` id — code supports all three; resolve at runtime (direct → CDN-prefix → storage).
4. `expires_in` semantics + refresh endpoint (seen `refreshToken` stored but refresh call not isolated; on 401 the tool just asks for re-`login`).
5. HLS video (`VUE_APP_HLS_API_URL`) — out of scope (no video download goal).
6. `is_open` gating: client skips `fetch contents` when `is_open != 1`; tool should surface closed modules as `[closed]` rather than fail.

## 10. My Courses tabs (sidebar mapping, verified from bundle)

Route `/courses` → chunk `course/list` → student component `StudentCourseList`
(`course/list-lecturer.3230e8e6.js`):

```js
tabs:[
  {text:"Active Courses",   qs:"[is_active][is]=1"},
  {text:"Inactive Courses", qs:"[is_active][is]=0"},
  {text:"All Courses",      qs:null}
]
```

It dispatches `course/fetchCourses` with `{include:["lecturer","lecturer.user"],
showStudent:false, courseOnly:true, filter}` where `filter` is the tab `qs`
string, joined as `filter[is_active][is]=1` in the query (same `se["e"]`
builder). The student home (`home/student`) uses the same filter shape on
`enrollsCourse/fetchCourses` (`/course/enrolls/courses`).

Tool mapping: REPL root = the three tabs (`active/ inactive/ all/`),
each tab = `GET /course/courses?include=lecturer,lecturer.user` with that
tab's filter (`filter[is_active][is]=1` / `=0` / none) — the same endpoint
the web page itself uses. (The first build wrongly used
`/course/enrolls/courses`, which paged to a bogus 200 rows; live check:
`[active] page 1: 17 items (server total: 17)`.) Other sidebar entries
(Homepage, To Do, Assignments, Exams, …) are out of scope.
Homepage data itself was not mapped (not needed for downloads).

## 9. Minimal tool call sequence

```
POST /user/auth/login {username,password}   # or paste access_token directly (F1 supports both)
GET  /login/me                              # verify
GET  /course/enrolls/courses?page[limit]=50&page[offset]=0
   (+ tab filter per §10: filter[is_active][is]=1 / =0 / none)
GET  /course/courses/{id}?include=modules,modules.activities,lecturer,lecturer.user,assistant,class
GET  /course/contents/?filter[contentable_id][is]={moduleId}&filter[contentable_type][is]=activities&sort=index&page[limit]=0
   (fallback contentable_type=modules if empty; also try course-text query for is_open courses)
GET  /course/content/{id}  (if list row lacks file URL)
GET  <file url> (stream, Bearer header forwarded for storage/CDN hosts)
```

Viewer flow: item with `contents_type=ppt` → `content` URL → download it directly
(same for pdf). Only if `content` is empty/non-file do `SKIP viewer-only`;
`--include-viewer` additionally dumps `text/html/url` as `.html`/`.url` files.
No per-page image scraping: both readers embed the ORIGINAL file, so no `page-001.jpg` needed
(unless a future bundle shows otherwise — then revisit).
