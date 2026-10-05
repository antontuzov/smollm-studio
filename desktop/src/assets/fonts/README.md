# Fonts

`inter-latin-wght.woff2` and `inter-latin-ext-wght.woff2` are **vendored copies**
of Inter Variable (weight axis only) from the `@fontsource-variable/inter`
package in `desktop/package.json`. They are vendored rather than imported from
`node_modules` for two reasons:

- the app must render identically with the network off, and
- only two of Inter's seven subsets are needed — 133 KB shipped instead of
  ~700 KB for a "lightweight" app.

`INTER-LICENSE.txt` is the SIL Open Font License that covers these files and
must stay next to them.

To refresh after bumping the package version:

```bash
cd desktop
cp node_modules/@fontsource-variable/inter/files/inter-latin-wght-normal.woff2 \
   src/assets/fonts/inter-latin-wght.woff2
cp node_modules/@fontsource-variable/inter/files/inter-latin-ext-wght-normal.woff2 \
   src/assets/fonts/inter-latin-ext-wght.woff2
cp node_modules/@fontsource-variable/inter/LICENSE src/assets/fonts/INTER-LICENSE.txt
```

The `@font-face` rules live at the top of `src/styles/globals.css`.
