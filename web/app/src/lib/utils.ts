import { createCn } from "cn/config";

/** Knows the font-size tokens of index.css, so text-meta and a text colour on one element both survive the merge. */
export const cn = createCn({ extend: { classGroups: { "font-size": [{ text: ["tag", "meta", "ui"] }] } } });
