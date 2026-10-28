import { createCssVariablesTheme } from "shiki/core";

/** One Shiki theme for both modes: colours resolve through the syntax tokens in globals.css (§5.1). */
export const xflowTheme = createCssVariablesTheme({ name: "xflow", variablePrefix: "--shiki-", fontStyle: true });
