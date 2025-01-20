# Klappstuhl.me

Klappstuhl.me is my personal website that features a public image hosting service.
This Website is partly based on [Rapptz's klappstuhl_me](https://github.com/Rapptz/jimaku)


# Install

Right now, Rust v1.74 or higher is required. To install just run `cargo build`.

In order to actually run the server the `static` directory needs to be next to the executable. Maybe in the future there'll be a way to automatically move it.

In order to create an admin account, run the `admin` subcommand.

# Configuration

Configuration is done using a JSON file. The location of the configuration file depends on the operating system:

- Linux: `$XDG_CONFIG_HOME/klappstuhl_me/config.json` or `$HOME/.config/klappstuhl_me/config.json`
- macOS: `$HOME/Library/Application Support/klappstuhl_me/config.json`
- Windows: `%AppData%/klappstuhl_me/config.json`

The documentation for the actual configuration options is documented in the [source code](src/config.rs).

## Data and Logs

The server also contains a database and some logs which are written to different directories depending on the operating system as well:

For data it is as follows:

- Linux: `$XDG_DATA_HOME/klappstuhl_me` or `$HOME/.local/share/klappstuhl_me`
- macOS: `$HOME/Library/Application Support/klappstuhl_me`
- Windows: `%AppData%/klappstuhl_me`

For logs it is as follows:

- Linux: `$XDG_STATE_HOME/klappstuhl_me` or `$HOME/.local/state/klappstuhl_me`
- macOS: `./logs`
- Windows: `./logs`

The data directory contains the database.

# License

AGPL-v3.
