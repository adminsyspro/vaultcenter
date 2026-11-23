import {expect} from '@playwright/test';

export async function selectOption(trigger, value) {
  await trigger.click();
  const list = trigger.page().getByRole('listbox');
  await expect(list).toBeVisible();
  const selector = await trigger.page().evaluate(value => `[data-value="${CSS.escape(value)}"]`, value);
  await list.locator(selector).click();
  await expect(list).toBeHidden();
}

export async function expectOptions(trigger, labels) {
  await trigger.click();
  await expect(trigger.page().getByRole('listbox').getByRole('option')).toHaveText(labels);
  await trigger.press('Escape');
}
