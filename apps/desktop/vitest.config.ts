import { configDefaults, defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "jsdom",
    // Styled accessibility fixtures need the real flex/grid blockification rules.
    css: {
      include: [
        /packages\/desktop-ui\/src\/(?:styles|peppy-tokens|avatar-palette\.generated)\.css$/,
        /apps\/desktop\/src\/(?:app|shell\/accent-theme)\.css$/,
      ],
    },
    exclude: [...configDefaults.exclude, "scripts/**/*.test.mjs"],
  },
});
