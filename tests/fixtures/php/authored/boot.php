<?php

declare(strict_types=1);

namespace App\Boot;

// gold: (i) named function declaration.
function launch(): void
{
}

// gold: (c) top-level call with no containing symbol.
launch();

// gold: (e) scoped via new.
$svc = new \App\Services\SurveyService();
$svc->launch();

// gold: (h) the word launch in this comment is not a use.
$label = 'launch';
// gold: (h) interpolation keeps the inner call as a use.
$text = "{$svc->launch()}";
