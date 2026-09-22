<?php

declare(strict_types=1);

namespace App\Members;

abstract class AbstractThing
{
    public const FIRST = 1, SECOND = 2;

    public static int $count = 0;

    public readonly string $title;

    public int $left, $right = 2;

    abstract public function describe(): string;

    public function __construct(private int $seed, public string $tag = 'x')
    {
        $anon = new class {
            public function hidden(): void
            {
            }
        };
    }
}
