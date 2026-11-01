# Research evidence: 1 November 2026

- `github.json`: public GitHub API snapshots, reduced to relevant fields. Five
  repository records, five licence files (base64 exactly as returned), and 23
  issue records with bodies, state, timestamps, comment counts and reactions.
- `search-counts.json`: exact public GitHub issue search URLs, queries and results.
  Three searches succeeded. Two returned HTTP 403 rate-limit errors; no count was
  invented or substituted for those failures.
- `code-snapshot.json`: baseline Git HEAD and SHA-256 hashes of important inspected
  files. The checkout was already dirty; this is not evidence of released behavior.

Retrieval timestamps are UTC inside each snapshot. Review date is 1 November 2026
in Asia/Kolkata. Counts change and include different reaction types. Search matches
are overlapping text results, not unique users or requests. Licence blobs are source
evidence, not a legal opinion about every edition of a product.

Vendor documentation was checked live through web tools and is linked next to the
claims in the two research documents. It was not archived here; live documentation
may change and may describe a newer release than an old issue or benchmark. The
earlier Lima memory results were not rerun and matching raw benchmark evidence was
not located under `field-test/` or `docs/validation/` during this review.

See [the validation review](../../2026-11-01-COMPETITIVE-VALIDATION.md) for conclusions,
limits and proposed tests. No product code was changed as part of this review.

## Issue state index

These are current API states, not claims about every affected or fixed release.

| Issue | Title | State | Reactions (all types) |
| --- | --- | --- | ---: |
| [dokploy #2154](https://github.com/Dokploy/dokploy/issues/2154) | Container auto sleep/scale to zero | open | 26 |
| [dokploy #2386](https://github.com/Dokploy/dokploy/issues/2386) | Support preview deployments from pre-built Docker images (built via CI/CD) | open | 24 |
| [dokploy #2462](https://github.com/Dokploy/dokploy/issues/2462) | Preview Deployments - Also run database migrations only for each Preview Deployment | open | 29 |
| [dokploy #319](https://github.com/Dokploy/dokploy/issues/319) | Ability to roll back service depoyments | closed | 69 |
| [dokploy #416](https://github.com/Dokploy/dokploy/issues/416) | Ability to backup to more destination types | open | 29 |
| [dokploy #711](https://github.com/Dokploy/dokploy/issues/711) | Custom build server | closed | 50 |
| [coolify #1996](https://github.com/coollabsio/coolify/issues/1996) | [Bug]: Docker-compose deployments from repositories don't work if they need additional files | closed | 18 |
| [coolify #2073](https://github.com/coollabsio/coolify/issues/2073) | [Bug]: Healthcheck not working as expected | closed | 2 |
| [coolify #2107](https://github.com/coollabsio/coolify/issues/2107) | [Bug]: Docker deployment not considering environment variables | closed | 16 |
| [coolify #2110](https://github.com/coollabsio/coolify/issues/2110) | [Bug]: Unexplained High CPU Usage Spike in Coolify Following Deployment Attempt | closed | 30 |
| [coolify #2378](https://github.com/coollabsio/coolify/issues/2378) | [Feature]: Add more granular permissions and user roles. | open | 17 |
| [coolify #2389](https://github.com/coollabsio/coolify/issues/2389) | [Feature]: Backup Manger in the UI | open | 38 |
| [coolify #2679](https://github.com/coollabsio/coolify/issues/2679) | [Bug]: Preview deployment of closed pull request still active | closed | 14 |
| [coolify #3079](https://github.com/coollabsio/coolify/issues/3079) | [Bug]: Environment (secrets) variables for Preview Deployments not working | closed | 9 |
| [coolify #3395](https://github.com/coollabsio/coolify/issues/3395) | [Bug]: PullHelperImageJob failed with: Error response from daemon: toomanyrequests | closed | 4 |
| [coolify #4483](https://github.com/coollabsio/coolify/issues/4483) | [Bug]: constant Gateway Timeout errors after deploy | closed | 12 |
| [coolify #4539](https://github.com/coollabsio/coolify/issues/4539) | [Bug]: Scheduled tasks not executing on schedule all of a sudden | closed | 1 |
| [coolify #5074](https://github.com/coollabsio/coolify/issues/5074) | [Bug]: Docker Cleanup stuck on "In Progress" | closed | 11 |
| [coolify #5450](https://github.com/coollabsio/coolify/issues/5450) | [BUG] SSL Postgres: FATAL: could not load private key file  | closed | 13 |
| [coolify #5685](https://github.com/coollabsio/coolify/issues/5685) | Coolify v5.x | open | 691 |
| [coolify #5933](https://github.com/coollabsio/coolify/issues/5933) | [Bug]: coolify does not start correctly when the machine/host restart | closed | 0 |
| [coolify #6656](https://github.com/coollabsio/coolify/issues/6656) | [Bug]: Next.js rebuild consumes all memory and crashes Coolify server | open | 4 |
| [coolify #7193](https://github.com/coollabsio/coolify/issues/7193) | [Bug]: Coolify Traefik Proxy No Longer Working After Recent Update | closed | 73 |
