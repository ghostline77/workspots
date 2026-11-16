import { buildApp } from "@ghostline/ui-core/tooling/build-ui";

await buildApp(".", process.argv.includes("--dev"));
