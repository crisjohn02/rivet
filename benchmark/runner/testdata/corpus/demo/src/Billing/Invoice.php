<?php

namespace App\Billing;

class Invoice
{
    public function total(): int
    {
        return $this->sum();
    }

    private function sum(): int
    {
        return 0;
    }
}
