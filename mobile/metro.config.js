const { getDefaultConfig } = require('expo/metro-config');
const path = require('node:path');

const config = getDefaultConfig(__dirname);

// Resolve the `@/*` path alias (configured in tsconfig.json) so Metro can
// find modules under src/ at bundle time. Without this, runtime imports
// like `@/db/goalsDb` silently fail and components never mount.
config.resolver.alias = {
  ...(config.resolver.alias || {}),
  '@': path.resolve(__dirname, 'src'),
};

module.exports = config;
