import assert from "node:assert/strict";
import { describe, it } from "node:test";
import remend from "remend";

describe("Markdown streaming math remend (Issue #18)", () => {
  it("preserves comparison operators like <b at stream tail when htmlTags remending is disabled", () => {
    // When text ends with an incomplete comparison like '<b' or '<y',
    // default remend treats it as an unclosed HTML tag (<b...) and strips it.
    const defaultStripped = remend("Formula: x <b");
    assert.equal(defaultStripped, "Formula: x");

    // With htmlTags: false, incomplete HTML tag stripping is disabled and the comparison is preserved.
    const htmlTagsDisabled = remend("Formula: x <b", { htmlTags: false });
    assert.equal(htmlTagsDisabled, "Formula: x <b");
  });
});
