# Self-hosted sync with `tanki sync-server`

tanki syncs only with a server you run yourself. It has no default server and
refuses AnkiWeb (`ankiweb.net` and its subdomains), because AnkiWeb's terms
exclude third-party clients. `tanki sync-server` starts Anki's own built-in
sync server (the same code as `anki --syncserver` and the `anki-sync-server`
binary, linked into tanki), so you have something to sync with. Anki's manual
calls third-party sync servers untested; this one is Anki's own server code,
at the version tanki is built against.

## Start the server

Write the users to a file that only you can read, one `username:password` per
line (`#` starts a comment):

```sh
install -m 600 /dev/null ~/.config/tanki/sync-users
printf 'alice:a-long-password\n' >> ~/.config/tanki/sync-users
tanki sync-server --data ~/tanki-sync-data --users-file ~/.config/tanki/sync-users
```

tanki refuses a users file that other users can read. Passwords never go on
the command line. Without `--users-file` the server reads `SYNC_USER1`,
`SYNC_USER2`, ... (and `PASSWORDS_HASHED`) from the environment, exactly as
Anki's own server does.

| Flag | Default | Meaning |
|---|---|---|
| `--host` | `127.0.0.1` | Address to bind. Anki's own server defaults to `0.0.0.0`; tanki binds loopback unless told otherwise. |
| `--port` | `8080` | TCP port. |
| `--data` | (required) | The server's own data folder; one subfolder per user. |
| `--users-file` | (none) | `username:password` lines, mode 0600. |

The server prints the address it listens on and runs until Ctrl-C.

### The data folder is never a profile folder

The server keeps its own copy of each user's collection and media. Clients
sync *into* it; never copy files into it, and never point it at an Anki
folder. tanki refuses, by path alone and without opening anything there:

- a folder that holds `collection.anki2`, `collection.media`, its media
  database or `prefs21.db` (a profile or an Anki base), or any folder inside
  one;
- anything under `~/.local/share/Anki2`, the Flatpak base
  `~/.var/app/net.ankiweb.Anki/data/Anki2`, `$XDG_DATA_HOME/Anki2` or
  `$ANKI_BASE`.

Use a new, empty folder.

## Plain HTTP: put it behind TLS or a private network

The server speaks plain HTTP. Passwords and your collection cross the network
unencrypted. Before other machines reach it, do one of these:

- **Private network** (simplest): run it on a machine in a Tailscale (or
  WireGuard) network and bind `--host` to that interface's address, or to
  `0.0.0.0` with a firewall that only admits the private network. Clients use
  `http://<tailnet-name-or-ip>:8080/`.
- **TLS reverse proxy**: keep `--host 127.0.0.1` and put Caddy, nginx or
  similar in front with a certificate. Clients use `https://sync.example.org/`.
  Allow large request bodies (a full upload is the whole collection; Anki's
  limit is `MAX_SYNC_PAYLOAD_MEGS`, 100 MB compressed by default) and use
  HTTP/1.1 towards the server.

## Point tanki at it

```sh
tanki sync login --url https://sync.example.org/ --username alice
tanki sync            # normal sync, then media
tanki sync status     # what a sync would do; changes nothing
```

The host key returned by the server is stored in
`~/.config/tanki/credentials.toml` (mode 0600); the URL and username go to
`~/.config/tanki/config.toml`. If the server redirects to a new address, tanki
checks it like a configured URL (never AnkiWeb) and saves it.

When a sync needs a one-way full sync, `tanki sync` exits with code 21 and asks
you to choose: `--upload` replaces the server's copy with this collection,
`--download` replaces this collection with the server's (after a backup).
Exit code 22 means log in again; 23 means sync is not configured.

## Point Anki desktop, AnkiDroid and AnkiMobile at it

All three let you set a custom sync server in their own settings, so one
server can replace AnkiWeb for every device:

- **Anki desktop**: Preferences, Syncing, "Self-hosted sync server", enter the
  URL, then log in with the username and password from your users file.
- **AnkiDroid**: Settings, Sync (Advanced), "Custom sync server", enter the
  URL for both collection and media sync if asked.
- **AnkiMobile**: Settings, Syncing, custom/self-hosted server, enter the URL.

Menu names change between versions; see each app's manual if they differ.
Clients must speak a sync protocol version the server supports, so keep
clients reasonably close to the Anki version tanki is built against.

## Switching a collection to this server

A collection's sync state is relative to the server it last synced with. The
first sync of a collection that has synced elsewhere before (for example with
AnkiWeb) is a one-way full upload or download, and every other device must
then be pointed at this server too. Syncing one collection alternately with
two servers forces a full sync each time it switches (verified in tanki's
tests). Pick one device's copy, upload it, and let the others download.

Media sync notices added, removed and replaced files. A file edited in place
with the same name is not noticed until something else in the media folder
changes (Anki behaves the same way).
