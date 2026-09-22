<?php

declare(strict_types=1);

namespace App\Services;

// gold: (a) first of two classes that each declare a same-name method.
// gold: (i) class constant and typed property for later kind coverage.
final class SurveyService
{
    // gold: (i) class constant declaration.
    public const DEFAULT_LABEL = 'survey';

    // gold: (i) typed property declaration.
    private string $label = 'survey';

    // gold: (a) first same-name method declaration.
    public function launch(): void
    {
        $this->label = self::DEFAULT_LABEL;
    }

    // gold: (d) scoped via this.
    public function relaunch(): void
    {
        $this->launch();
    }
}
