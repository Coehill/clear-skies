> [!WARNING]
> Environment Canada now provides its own [forecast API](https://api.weather.gc.ca/collections/citypageweather-realtime?lang=en). It's still experimental, but worth considering before using Clear Skies for a new project.

# Clear Skies 🌤️

A simple weather API backed by Environment Canada's [Current Condition & Forecast Atom feeds](https://www.canada.ca/en/environment-climate-change/services/weather-general-tools-resources/weatheroffice-online-services/data-services.html#toc1).

> [!NOTE]
> Clear Skies is a hobby project. Expect ongoing development and maintenance to be very much correlated to how much value it brings me as a learning resource and as a data source for my other projects.
>
> You are welcome to use the instance hosted on [clear-skies.ca](https://clear-skies.ca) (thanks to Fly.io's free tier) with the understanding that it comes with no uptime guarantees. Please host your own instance if you need something more robust.

## Getting started

- [OpenAPI Specification](https://clear-skies.ca/openapi.json)
- [Swagger UI](https://clear-skies.ca/swagger-ui)

## Developing

### Building

Clear Skies uses [Nix](https://nixos.org/) to provide a consistent build and development environment. The most straightforward way to install it is using Determinate Systems' [`nix-installer`](https://github.com/DeterminateSystems/nix-installer#usage).

Once you have Nix installed, you can run:

- `nix develop` to enter a shell with pinned versions of the Rust toolchain, Node.js, and Yarn.
- `nix build` to compile a release version of the `weather-server` binary
- `nix build ".#container"` to build an OCI image which runs `weather-server` (this is what [gets pushed to Fly.io](https://github.com/davidcornu/clear-skies/blob/8fe4c129fe693596b54ab67270a97b1d9ca14587/.github/workflows/deploy.yml#L18-L23))

### Environment Canada feeds

`weather-dev cache-feeds` downloads coordinate-based RSS feeds from all entries
in each province and territory listing. Use a fresh cache directory before
running `build-mapping` to avoid mixing old and new feed files.

When correcting a catalogue entry, verify its coordinates against ECCC's
province listing and update both `weather-dev/data/coordinates.json` and the
entry in `weather-lib/src/locations/data.rs`. Check the returned location name,
not just the HTTP status.

### Static Assets

The HTML, JS, and CSS files in the `weather-server/src/static` directory are bundled into the `weather-server` binary using [`rust-embed`](https://lib.rs/crates/rust-embed). In debug builds these are served from the filesystem so you don't need to rebuild the binary every time you change them.

Both `assets/try.js` and `assets/try.css` are build artifacts, which are committed as-is to avoid needing to run a JS build step. If you need to make changes, `cd frontend` and then run `yarn install` and `yarn build`.

## Repository Structure

- `weather-dev` is a binary package that contains development commands, notably to pull down feeds listed on Environment Canada's province forecast pages and generate `weather-lib/src/locations/data.rs`.
- `weather-lib` is a library package that contains all the necessary logic to turn Environment Canada feeds into a machine-readable format.
- `weather-sync` is a library package that provides a caching HTTP client for Environment Canada feeds.
- `weather-server` is a binary package for the [Dropshot](https://lib.rs/crates/dropshot)-powered API server which runs on [clear-skies.ca](https://clear-skies.ca).
- `frontend` contains the [Svelte](https://svelte.dev/) components that power the demo on the home page.
