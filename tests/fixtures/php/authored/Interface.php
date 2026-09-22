<?php

declare(strict_types=1);

namespace App\Contracts;

interface Named
{
    public const KIND = 'named';

    public function name(): string;

    public function set(int $value): void;
}
