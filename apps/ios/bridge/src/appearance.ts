import { readFile } from "node:fs/promises";

export type Appearance = { style: string; layout: string; lightTheme: string; darkTheme: string };
export const defaultAppearance: Appearance = { style: "default", layout: "minimal", lightTheme: "mast-light", darkTheme: "mast-dark" };
/** Project only presentation fields; never expose other settings. */
export async function readAppearance(path: string): Promise<Appearance> {
  try {
    const data = await readFile(path, "utf8");
    if (data.length > 2_000_000) return defaultAppearance;
    const value = JSON.parse(data);
    const style = ["default", "industrial", "horizon"].includes(value.uiStyle) ? value.uiStyle : "default";
    const layout = ["minimal", "compact", "guided"].includes(value.uiLayout) ? value.uiLayout : "minimal";
    const allowed = new Set(["mast-light", "mast-dark", "industrial-light", "industrial-dark", "horizon-light", "horizon-dark"]);
    const light = value.themeSelection?.light, dark = value.themeSelection?.dark;
    return { style, layout, lightTheme: allowed.has(light) ? light : "mast-light", darkTheme: allowed.has(dark) ? dark : "mast-dark" };
  } catch { return defaultAppearance; }
}
