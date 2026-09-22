<?php

declare(strict_types=1);

namespace App\Reporting;

// gold: (b) alias import whose spelling differs from the class target.
use App\Services\SurveyService as SurveySvc;
// gold: (f) direct import used by an explicit parameter type.
use App\Services\SurveyService;

// gold: (a) second class with a same-name method.
final class ReportService
{
    // gold: (a) second same-name method declaration.
    public function launch(): void
    {
    }

    // gold: (b) call through the alias binding.
    public function runAlias(): void
    {
        $svc = new SurveySvc();
        $svc->launch();
    }

    // gold: (f) scoped via explicit parameter type.
    public function runTyped(SurveyService $svc): void
    {
        $svc->launch();
    }

    // gold: (g) untyped receiver stays name_match.
    public function runUnknown(): void
    {
        $x->launch();
    }
}
