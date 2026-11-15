import { runCommand } from "../core/ipc";
import { createSiteKeySync } from "./siteModelSync";

/** Shared across drawers, model dialogs and batch workers to coalesce duplicate account requests. */
export const syncSiteAccountKeys = createSiteKeySync(runCommand);
