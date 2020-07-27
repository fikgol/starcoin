//! account: bob

//! new-transaction
//! sender: bob
script {
    use 0x1::Math::sqrt;
    fun main(_signer: &signer) {
        assert(sqrt(0) == 0, 0);
        assert(sqrt(1) == 1, 1);
        assert(sqrt(2) == 1, 1);
        assert(sqrt(3) == 1, 1);

        assert(sqrt(4) == 2, 2);
        assert(sqrt(5) == 2, 2);

        assert(sqrt(9) == 3, 3);
        assert(sqrt(15) == 3, 3);
        assert(sqrt(16) == 4, 5);
    }
}
// check: EXECUTED