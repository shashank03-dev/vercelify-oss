# vfy command reference

`vfy [-s SERVER] COMMAND …`. APP is an app's name or Coolify uuid. Exit status is
non-zero on any error, with the reason on stderr prefixed `vfy:`.

| Command | Does | Changes anything? |
| --- | --- | --- |
| `vfy servers` | Paired servers; `*` marks the one selected in the app | no |
| `vfy apps` (`ls`) | Every app with its status and URL | no |
| `vfy deploy APP` | Deploy the latest commit now | **yes** |
| `vfy logs APP [LINES]` | Runtime logs (default 200 lines) | no |
| `vfy rollback APP COMMIT` | Put an earlier commit back live | **yes** |
| `vfy env pull APP [FILE]` | Production variables to FILE (`.env`), private; refuses to overwrite unless `VFY_FORCE=1` | writes a secret file |
| `vfy open APP` | Open the app's URL | no |
| `vfy status` (`doctor`) | The server's health checks | no |
| `vfy alerts` | What the server reported lately | no |
| `vfy backup` | Start a backup now | **yes** |
| `vfy sleep APP [MINUTES\|off]` | Stop APP after MINUTES without visits (default 30) | **yes** |
| `vfy config check [DIR]` | What DIR's `vercel.json` (+ `vercelify.json`) would do | no |
| `vfy config push APP [DIR]` | Apply DIR's vercel.json to APP | **yes** |
| `vfy config sync APP` | Re-read APP's vercel.json from GitHub | **yes** |
| `vfy config show\|rm APP` | Its rules and where they came from / remove them | `rm`: **yes** |
| `vfy crons APP [run PATH]` | Cron jobs and their last runs / run one now | `run`: **yes** |
| `vfy why APP` | AI: why APP's last deployment failed and how to fix it | no |
| `vfy why --log FILE\|-` | The same for any build log, local Ollama, no server | no |

### On servers running Vercelify's own engine

A main server paired without a Coolify URL runs the platform engine; there APP is an
app's name or id, and these change or add to the commands above:

| Command | Does | Changes anything? |
| --- | --- | --- |
| `vfy deploy APP [--no-follow]` | Deploy and stream the build log; exits 1 if the build fails | **yes** |
| `vfy logs APP [LINES] [-f]` | The app's own output; `-f` keeps following it | no |
| `vfy rollback APP COMMIT\|#N` | Back to an earlier live deployment, by commit (4+ characters) or number | **yes** |
| `vfy restart APP` | Restart the running app (compose sidecars too) | **yes** |
| `vfy env set APP K=V… [--plain] [--build]` | Set variables (secret unless `--plain`; `--build` also at build time) | **yes** |
| `vfy env push APP [FILE]` | Import a `.env` file (default `.env`); existing keys are overwritten | **yes** |
| `vfy services` | The service catalogue: Umami, Plausible, MinIO, Unleash, Uptime Kuma, Vaultwarden | no |
| `vfy services add ID [NAME]` | Create and deploy one, its secrets generated | **yes** |

## vfy why options

```
--log FILE|-            a build log instead of an app (- = stdin)
--vercel-json FILE      add vercel.json as context (first 1500 characters)
--provider P            server (default for APP), ollama (default for --log), gemini
```

Environment: `VFY_AI_MODEL` (default `gemma4:e2b`), `VFY_OLLAMA_URL`
(default `http://localhost:11434`), `VFY_GEMINI_MODEL` (default `gemma-4-26b-a4b-it`;
`gemma-4-31b-it` also works), `VFY_GEMINI_URL`, `GEMINI_API_KEY`, `VFY_AI_PROVIDER`,
`VFY_SETTINGS`, `NO_COLOR`.

Example output (the answer streams token by token):

```text
$ vfy why blog
blog failed at 4b1d000 (2026-10-23T08:12:00Z)
Asking gemma4:e2b on laptop…

**Cause:** `npm run build` fails: app/page.tsx imports ../components/Header, but the file is header.tsx and Linux file names are case-sensitive.
**Fix:** change the import in app/page.tsx to "../components/header".
**Check:** `npm run build`

— gemma4:e2b on laptop, via Ollama · 9.8 tokens/s
```

The answer above is illustrative; the model writes its own words each time.
