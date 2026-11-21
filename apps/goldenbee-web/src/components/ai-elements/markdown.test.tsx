import { render } from "@testing-library/react"
import { describe, expect, it } from "vitest"

import { Markdown } from "@/components/ai-elements/markdown"

describe("Markdown", () => {
  it("renders assistant markdown instead of literal markers", () => {
    const { container } = render(
      <Markdown>{"**bold text** and `inline code`"}</Markdown>,
    )

    expect(
      container.querySelector('[data-streamdown="strong"]'),
    ).toHaveTextContent("bold text")
    expect(
      container.querySelector('[data-streamdown="inline-code"]'),
    ).toHaveTextContent("inline code")
  })
})
