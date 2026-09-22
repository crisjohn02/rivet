<?php

declare(strict_types=1);

namespace App\Concerns;

trait Greets
{
    public const SALUTE = 'hi';

    public function greet(): string
    {
        return 'hello';
    }
}
