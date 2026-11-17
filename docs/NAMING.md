<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-naming.svg" alt="Naming. Why is it called scrutate, and how was that checked?" width="880">
</p>

# Project name: **Scrutate**

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Research](RESEARCH.md)

<!-- End of head. -->

*scrutate*: an English verb from Latin *scrūtor / scrūtārī*, "to search carefully, search into or
out, examine thoroughly, investigate" (Lewis & Short, *A Latin Dictionary*, 1879, as cited by
Wiktionary's entry for *scrutor*, which lists English *scrutate* among its descendants next to
*scrutinize*). The CLI, the crate and the Python package are `scrutate`; the index directory is
`.scrutate/`.

## Why this name

- It says what the tool does, search a codebase and examine it closely, without being a generic
  "code search" phrase.
- It is a real word (SKROO-tayt), easy to say, type and remember, with the sense of *scrutiny*.
  It is rare enough in current English that nothing in the registries a developer tool could
  collide with uses it (evidence below).
- It is a verb, so it works in sentences ("scrutate the repo for callers of `refresh_token`").

## Method

Thirty candidates came from real words about search or insight, coined blends, and Rust- and
grep-themed names. Each was checked on **2026-11-08**: `tools/name_research.py` queried the
registries and GitHub, and the Homebrew and web checks were made by hand for the finalist only.
Facts below that are not in `docs/name_research.json` (Homebrew, web, WHOIS and trademark
searches, the re-check of 2026-11-09, notes on other products) come from those searches by hand
and are recorded only in this file.

| Source | How | Result recorded |
|---|---|---|
| crates.io | `GET /api/v1/crates/<name>` | taken / free |
| PyPI | `GET /pypi/<name>/json` | taken / free |
| npm | `GET registry.npmjs.org/<name>` | taken / free |
| GitHub repositories | search API, `<name> in:name`, 30 most-starred inspected | number of name matches; most-starred repository named exactly the candidate |
| GitHub user/organisation | `GET /users/<name>` | **not verifiable** here (the call failed for every candidate; its status is not recorded) |
| Homebrew | `formulae.brew.sh/api/formula/<name>.json` (finalist only) | no formula |
| Web search | finalist only | software products or companies with the same or a confusable name |

Raw evidence: `docs/name_research.json`, one record per candidate with the HTTP status codes and
the top GitHub match.

## Evidence table

| Candidate | crates.io | PyPI | npm | GitHub name matches | Most-starred exact-name repo | Verdict |
|---|---|---|---|---|---|---|
| **scrutate** | free | free | free | 3 (all *Scrutateur...*, none exact) | none | **selected** |
| greplex | free | free | free | 0 | none | runner-up: confusable with the existing tools *greple* (Perl grep) and *Greptile* |
| grepnova | free | free | free | 3 | none, but a `grepNova` GitHub account owns `grepNova/ruby`, `grepNova/proyectos` | rejected: account collision |
| sondel | free | free | free | 28 | none | rejected: a surname, no meaning for users |
| codesonde | free | free | free | 1 | none | rejected: weak, hard to say |
| sondex | free | free | free | 7 | `josuekot23/sondex` ★0 | rejected |
| grepwise | free | free | free | 3 | `ozkanpakdil/grepwise` ★1 ("Splunk alternative") | rejected: exact-name project |
| codeken | free | free | free | 52 | `all3xfx/codeken` ★0 | rejected: exact-name project |
| lexicode | free | free | free | 46 | `wobh/lexicode` ★14 | rejected |
| querent | free | taken | taken | 45 | `teogor/querent` ★5 | rejected |
| pellucid | free | free | taken | 69 | `meppent/pellucid` ★48 (Rust EVM decompiler) | rejected: Rust-ecosystem collision |
| kenning | free | taken | free | 115 | `antmicro/kenning` ★149 | rejected |
| codequill | free | free | taken | 20 | `The-Best-Codes/codequill` ★25 | rejected |
| sourcelight | free | free | taken | 39 | `philosophysystems/sourcelight` ★0 | rejected |
| spelunk | free | taken | taken | 616 | `Rich-Harris/spelunk` ★30 | rejected |
| spelunker | free | taken | taken | 163 | `wowserhq/spelunker` ★23 | rejected |
| percept | free | taken | taken | 32 750 | none exact in top 30 | rejected: generic |
| greptile | free | taken | taken | 1 317 | `ncornette/greptile` ★24; also an existing company | rejected |
| ferrograph | taken | free | free | 10 | `GarthDB/ferrograph` ★4 ("Rust code intelligence") | rejected: direct collision |
| dowser | taken | taken | free | 101 | `catfact/dowser` ★16 | rejected |
| syntaxis | taken | taken | free | 160 | `shobrook/syntaxis` ★18 | rejected |
| celerity | taken | taken | taken | 257 | `jarib/celerity` ★204 | rejected |
| lodestar | taken | taken | taken | 665 | `ChainSafe/lodestar` ★1 433 | rejected |
| quarry | taken | taken | taken | 1 947 | `barneygale/quarry` ★548 | rejected |
| sift | taken | taken | taken | 7 358 | `svent/sift` ★1 656 (a grep alternative) | rejected |
| scour | taken | taken | taken | 859 | `scour-project/scour` ★878 | rejected |
| hark | taken | taken | taken | 4 177 | `otalk/hark` ★571 | rejected |
| tessera | taken | taken | taken | 10 694 | `tessera-metrics/tessera` ★1 172 | rejected |
| augur | taken | taken | taken | 1 437 | `augurlabs/augur` ★695 | rejected |
| rummage | taken | taken | taken | 227 | `facelessuser/Rummage` ★87 (grep GUI) | rejected |

## Finalist checks for *scrutate*

- crates.io, PyPI, npm: no package of that name (HTTP 404 on 2026-11-08).
- GitHub: no repository named `scrutate`; the three name matches are French `Scrutateur` projects
  (an unrelated word).
- Homebrew: no formula (`formulae.brew.sh` returns not-found, and its formula for `ripgrep`, so
  the check works).
- Web search for "scrutate" software: no product or project. The nearest name is **Scrut**
  (Scrut Automation, a compliance-automation SaaS), another word in an unrelated market.

## Re-check on 2026-11-09

- crates.io, PyPI, npm: still no package named `scrutate` (HTTP 404).
- GitHub organisation `scrutate`: did not exist then (the authenticated `GET /users/scrutate`
  returned 404). The project claimed it on 2026-11-13; the repository is
  github.com/scrutate/scrutate.
- Domains (WHOIS): `scrutate.com` **is registered** (created 2026-06-22, registrar Cloudflare;
  WHOIS hides the owner, and whether it is this project's authors' was not checked).
  `scrutate.io`, `scrutate.org` and `scrutate.net` returned "not found" (unregistered then).
  `scrutate.dev` and `scrutate.app`: the registry's WHOIS server did not answer this client, so
  their status is unknown.
- Trademarks: a web search for "Scrutate" as a software product, company or mark found none. The
  closest marks are **SCRUT**, **SCRUT.IO** and **THE SCRUT PLATFORM**, held by Scrut Automation
  Inc. (security and compliance software; listed on Justia's trademark pages): another word, but
  in software, so a confusion analysis belongs in the legal review. The registers searched on
  2026-11-13 (below) show the US filings of these marks abandoned and the UK ones registered.

## Trademark knockout search on 2026-11-13

A lay knockout search of the official registers, for the name and close variants. Class 9 covers
downloadable software and class 42 software as a service; both matter here.

| Register | Query | Result |
|---|---|---|
| USPTO Trademark Search, General search, live and dead | `CM:*scrutat*` (SCRUTATE, SCRUTATOR, SCRUTATION) | 0 |
| USPTO | sound-alike regex `CM:/.*s[ck]+r[ouw]+t+[aei]+t.*/` | 0 (the control `CM:/.*s[ck]+r[ouw]+t+[aeiou]+[nt].*/` found 46, so the regex form works) |
| USPTO | `CM:*scrut*` | 53; `CM:*scrut* AND LD:true AND (IC:009 OR IC:042)`: 9 live marks in class 9 or 42 |
| TMview (EUIPO and the national offices it carries), "Contains" | `scrutate` | 0 |
| TMview | `scrut*` | 175, all read; none is SCRUTATE |
| WIPO Global Brand Database, "Embedded" | `scrutate`, `scrutat` | 0 and 0; the fuzzy search found nothing closer than SCRUTARE |
| UKIPO, "Contains String", all statuses | `scrut` | 24, all read; none is SCRUTATE |
| IP Australia, part match | `scrut` | 9; none is SCRUTATE |

Not searched directly: CIPO (Canada), JPO J-PlatPat (Japan) and US state registers; TMview's
Canadian and Japanese records for `scrut*` hold no SCRUTATE. Also on 2026-11-13: crates.io, PyPI,
npm and Homebrew return HTTP 404 for `scrutate` and `scrutator`, UK Companies House has no company
of that name, and a web search found no product or company called Scrutate.

No register searched holds SCRUTATE or any mark starting SCRUTAT, in any class, live or dead. The
near marks, for counsel to rate:

- **SCRUTINY**, US 99613386, pending, class 9, "Downloadable software development tools for code
  review" (Five Rivers Development LLC): the closest by goods.
- **SCRUT**, **SCRUT.IO**, **SCRUT AUTOMATION** and **THE SCRUT PLATFORM** (Scrut Automation
  Inc.): registered in the UK (UK00004112700, UK00004113041, UK00004113052, UK00004113065; classes
  9 and 42) and India (5185038, class 9). Every US filing of the family is abandoned (98766403,
  98179232, 98076953, 98076950, 98817425).
- **SCRUTINIZER**, registered in the US (86684376, class 9; 87289586, class 42), the UK, Australia
  and Japan.
- Recent European filings: Scrutor (Austria, 35 and 42), Scrutoir (France, 9, 41, 42 and 45),
  ScrutIA (France, 9 and 42).

## What is *not* verified: read before relying on the name

- **Legal clearance**: the registers above had a lay knockout search only, and the name has
  **not** been legally cleared. Counsel must give a clearance opinion before commercial use,
  rating the near marks above, SCRUTINY and the SCRUT family first.
- **The `.com` domain** is registered; unless it is already the authors', a product site needs
  another domain or a purchase.
- **Global uniqueness** is not claimed: the checks cover the listed registries on the listed dates
  and nothing else.
