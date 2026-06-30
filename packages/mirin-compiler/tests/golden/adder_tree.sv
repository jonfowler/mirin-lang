module adder_tree #(parameter int N) (
    input  logic [7:0] v [0:N-1],
    output logic [7:0] result
);
    logic [7:0] acc;
    logic [7:0] x;
    always_comb begin
        acc = 0;
        for (int __i0 = 0; __i0 < N; __i0++) begin
            x = v[__i0];
            acc = (acc + x);
        end
    end
    function automatic int headroom(input int n);
        int bits;
        bits = 8;
        for (int i = 0; i < n; i++) begin
            bits = (bits + 1);
        end
        return bits;
    endfunction
    localparam int w = headroom(N);
    logic [w-1:0] wide;
    logic [w-1:0] __block_1;
    assign __block_1 = type(__block_1)'(acc);
    assign wide = __block_1;
    logic [7:0] __block_2;
    assign __block_2 = type(__block_2)'(wide);
    assign result = __block_2;
endmodule
