import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { createElement } from 'react';
import type { ServiceClient } from '../src/api/client';
import { HelpView } from '../src/views/Help';

const client = {} as ServiceClient;

describe('help', () => {
  it('renders every common fix', () => {
    const html = renderToStaticMarkup(createElement(HelpView, {
      client,
      serviceDown: false,
      onRefresh: () => {},
    }));
    for (const title of [
      'The drive letter is missing',
      'Copies start fast, then slow down',
      'My local disk is filling up',
      'Google sign-in expired or failed',
      'The drive shows a different size than my disk',
    ]) {
      expect(html).toContain(title);
    }
  });
});
